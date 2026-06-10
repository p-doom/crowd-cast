//! Linux tray implementation using `ksni` (pure-Rust StatusNotifierItem over D-Bus).
//!
//! Mirrors the structure of `tray_macos.rs`: all business logic lives in `tray.rs`;
//! this file only renders the native menu and collects user actions. Unlike macOS
//! (which drives a Cocoa run loop via the dmikushin/tray C library), Linux speaks the
//! freedesktop StatusNotifierItem (SNI) protocol — the modern, display-server-agnostic
//! (X11 + Wayland) tray standard — entirely in Rust via zbus. No GTK main loop and no
//! `libappindicator`/`libayatana` C runtime dependency.
//!
//! Threading: `ksni`'s `blocking` API runs the SNI service on its own background thread,
//! so `init()`/`update()`/`poll()` stay synchronous and need no tokio context. Menu
//! `activate` closures run on that service thread and push a `TrayAction` onto an mpsc
//! channel; `poll()` (called from the shared `TrayApp` loop) drains it — the same
//! "callback sets a flag, poll() reads it" shape as the macOS atomic-bool bridge.
//!
//! Coverage note: SNI only renders where a `StatusNotifierHost` is running (KDE, LXQt,
//! XFCE/MATE/Cinnamon, COSMIC, Ubuntu-GNOME, sway, Hyprland/river+Waybar, ...). On
//! vanilla GNOME (no AppIndicator extension) or a bare tiling WM with no SNI-capable bar,
//! the item registers but stays invisible. `watcher_online`/`watcher_offline` track that
//! so the agent keeps running headlessly and we can surface guidance (see `poll`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use ksni::blocking::{Handle, TrayMethods};
use tracing::{info, warn};

use super::platform_tray::{
    PlatformTray, PlatformTrayPoll, TrayAction, TrayDisplayState, TrayIconPaths, TrayIconState,
};

/// Set by the SIGINT handler (via `request_tray_exit`) to break the tray loop, the Linux
/// analog of macOS's `tray_exit()`. `poll()` consumes it and returns `Exit`.
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask the running tray to exit its loop. Called from the SIGINT handler in `main.rs`.
pub fn request_tray_exit() {
    EXIT_REQUESTED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// SNI item model (lives on the ksni service thread)
// ---------------------------------------------------------------------------

/// The StatusNotifierItem model. Holds the current display state plus the precomputed
/// per-state icons; `ksni` re-reads `menu()`/`icon_pixmap()` from it after every
/// `Handle::update`. Menu activations push onto `tx`; `host_present` mirrors whether a
/// StatusNotifierHost is currently rendering us.
struct TrayModel {
    icon_state: TrayIconState,
    status_text: String,
    account_text: String,
    sign_action_text: String,
    auth_enabled: bool,
    can_start: bool,
    can_stop: bool,
    uploads_text: String,
    can_check_updates: bool,

    icon_idle: ksni::Icon,
    icon_recording: ksni::Icon,
    icon_blocked: ksni::Icon,

    tx: Sender<TrayAction>,
    host_present: Arc<AtomicBool>,
}

impl TrayModel {
    fn current_icon(&self) -> ksni::Icon {
        match self.icon_state {
            TrayIconState::Idle => self.icon_idle.clone(),
            TrayIconState::Recording => self.icon_recording.clone(),
            TrayIconState::Blocked => self.icon_blocked.clone(),
        }
    }
}

impl ksni::Tray for TrayModel {
    // Left-click opens the menu (the menu is crowd-cast's entire UI), matching the
    // behavior users expect on KDE/AppIndicator.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "crowd-cast Agent".into()
    }

    // Always "Active" so hosts that hide Passive items still show us.
    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.current_icon()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "crowd-cast Agent".into(),
            description: self.status_text.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};

        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // Status line (disabled label).
        items.push(
            StandardItem {
                label: self.status_text.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        );

        // Account line (disabled label), shown only when signed in.
        if !self.account_text.is_empty() {
            items.push(
                StandardItem {
                    label: self.account_text.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Start Recording".into(),
                enabled: self.can_start,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::StartRecording);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Stop Recording".into(),
                enabled: self.can_stop,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::StopRecording);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Delete last 10 minutes".into(),
                enabled: true,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::Panic);
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: self.uploads_text.clone(),
                enabled: true,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::ToggleUploads);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: self.sign_action_text.clone(),
                enabled: self.auth_enabled,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::SignIn);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Settings".into(),
                enabled: true,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::Settings);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Check for Updates".into(),
                enabled: self.can_check_updates,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::CheckForUpdates);
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);

        items.push(
            StandardItem {
                label: "Quit".into(),
                enabled: true,
                activate: Box::new(|m: &mut Self| {
                    let _ = m.tx.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );

        items
    }

    fn watcher_online(&self) {
        self.host_present.store(true, Ordering::SeqCst);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.host_present.store(false, Ordering::SeqCst);
        // Keep the service alive and re-register when a host returns (e.g. a Waybar
        // restart, or the user enabling the GNOME AppIndicator extension). Returning
        // false would tear the tray down permanently.
        true
    }
}

// ---------------------------------------------------------------------------
// Icon conversion (PNG on disk -> ARGB32 pixmap for SNI)
// ---------------------------------------------------------------------------

/// Load a PNG (generated by `tray.rs::create_tray_icons`) and convert it to the ARGB32,
/// network-byte-order pixmap that the StatusNotifierItem `IconPixmap` property expects.
/// Falls back to a 1x1 transparent pixel if the file can't be read/decoded so the tray
/// still comes up.
fn load_argb_icon(path: &Path) -> ksni::Icon {
    match image::open(path) {
        Ok(img) => argb_icon_from_rgba(&img.to_rgba8()),
        Err(e) => {
            warn!("Failed to load tray icon {:?}: {} — using blank icon", path, e);
            ksni::Icon {
                width: 1,
                height: 1,
                data: vec![0, 0, 0, 0],
            }
        }
    }
}

/// Convert an RGBA image to a ksni `Icon`. SNI pixmaps are ARGB32 in network (big-endian)
/// byte order, i.e. each pixel's bytes are `[A, R, G, B]`; `image` gives `[R, G, B, A]`,
/// so a right-rotate by one byte does the conversion in place.
fn argb_icon_from_rgba(img: &image::RgbaImage) -> ksni::Icon {
    let width = img.width() as i32;
    let height = img.height() as i32;
    let mut data = img.as_raw().clone();
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1);
    }
    ksni::Icon {
        width,
        height,
        data,
    }
}

// ---------------------------------------------------------------------------
// LinuxTray (implements PlatformTray)
// ---------------------------------------------------------------------------

pub struct LinuxTray {
    icon_idle: ksni::Icon,
    icon_recording: ksni::Icon,
    icon_blocked: ksni::Icon,
    handle: Option<Handle<TrayModel>>,
    action_tx: Sender<TrayAction>,
    action_rx: Receiver<TrayAction>,
    host_present: Arc<AtomicBool>,
    last_host_logged: Option<bool>,
}

impl LinuxTray {
    pub fn new(icon_paths: &TrayIconPaths) -> Result<Self> {
        let (action_tx, action_rx) = mpsc::channel();
        Ok(Self {
            icon_idle: load_argb_icon(&icon_paths.idle),
            icon_recording: load_argb_icon(&icon_paths.recording),
            icon_blocked: load_argb_icon(&icon_paths.blocked),
            handle: None,
            action_tx,
            action_rx,
            host_present: Arc::new(AtomicBool::new(false)),
            last_host_logged: None,
        })
    }
}

impl PlatformTray for LinuxTray {
    fn init(&mut self) -> Result<()> {
        EXIT_REQUESTED.store(false, Ordering::SeqCst);

        let model = TrayModel {
            icon_state: TrayIconState::Idle,
            status_text: "Status: Idle".to_string(),
            account_text: String::new(),
            sign_action_text: "Sign in with Google".to_string(),
            auth_enabled: true,
            can_start: true,
            can_stop: false,
            uploads_text: "Pause Uploads".to_string(),
            can_check_updates: false,
            icon_idle: self.icon_idle.clone(),
            icon_recording: self.icon_recording.clone(),
            icon_blocked: self.icon_blocked.clone(),
            tx: self.action_tx.clone(),
            host_present: self.host_present.clone(),
        };

        let handle = model
            .spawn()
            .map_err(|e| anyhow!("Failed to start StatusNotifierItem tray service: {e}"))?;
        self.handle = Some(handle);
        info!("Linux StatusNotifierItem tray service started");
        Ok(())
    }

    fn poll(&mut self) -> PlatformTrayPoll {
        // SIGINT (or an explicit exit request) takes priority.
        if EXIT_REQUESTED.swap(false, Ordering::SeqCst) {
            return PlatformTrayPoll::Exit;
        }

        // Log host-presence transitions once. This is the hook for graceful degradation:
        // when no host is rendering us, the agent keeps running but the user has no tray.
        let present = self.host_present.load(Ordering::SeqCst);
        if self.last_host_logged != Some(present) {
            self.last_host_logged = Some(present);
            if present {
                info!("StatusNotifier host present — tray icon is visible");
            } else {
                warn!(
                    "No StatusNotifier host is rendering the tray (e.g. vanilla GNOME \
                     without the AppIndicator extension, or a status bar without SNI \
                     support). The agent keeps running; control via tray is unavailable \
                     until a host appears."
                );
            }
        }

        // If the service shut itself down, treat it as an exit so the loop unwinds cleanly.
        if self.handle.as_ref().map(|h| h.is_closed()).unwrap_or(false) {
            return PlatformTrayPoll::Exit;
        }

        match self.action_rx.try_recv() {
            Ok(action) => PlatformTrayPoll::Action(action),
            Err(_) => PlatformTrayPoll::None,
        }
    }

    fn update(&mut self, state: &TrayDisplayState) {
        let Some(handle) = self.handle.as_ref() else {
            return;
        };

        let icon_state = state.icon_state;
        let status_text = state.status_text.clone();
        let account_text = state.account_text.clone();
        let sign_action_text = state.sign_action_text.clone();
        let auth_enabled = state.auth_action_enabled;
        let can_start = state.can_start;
        let can_stop = state.can_stop;
        let uploads_text = state.uploads_text.clone();
        let can_check_updates = state.can_check_updates;

        // Runs on the ksni service thread; ksni re-renders icon + menu afterwards.
        handle.update(move |m: &mut TrayModel| {
            m.icon_state = icon_state;
            m.status_text = status_text;
            m.account_text = account_text;
            m.sign_action_text = sign_action_text;
            m.auth_enabled = auth_enabled;
            m.can_start = can_start;
            m.can_stop = can_stop;
            m.uploads_text = uploads_text;
            m.can_check_updates = can_check_updates;
        });
    }

    fn prepare_for_restart(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }

    fn exit(&mut self) {
        EXIT_REQUESTED.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }
}
