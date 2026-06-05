//! Windows setup wizard — a native Win32 app picker built with native-windows-gui.
//!
//! Mirrors the macOS wizard contract: list capturable apps, let the user select
//! targets + toggle capture-all / start-on-login, then (on Save) update and save
//! the config so the re-exec in `main` sees `setup_completed = true`.
//!
//! The app list is a ListView with real checkboxes (LVS_EX_CHECKBOXES). nwg
//! doesn't expose checkbox state, so it's driven via raw Win32 SendMessage on
//! the control's HWND (see the `lv` module).

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use native_windows_gui as nwg;
use tracing::info;

use super::autostart::{disable_autostart, enable_autostart, AutostartConfig};
use super::wizard_gui::WizardResult;
use crate::config::Config;

/// Raw Win32 helpers for ListView checkbox state (nwg has no checkbox API).
mod lv {
    use winapi::shared::minwindef::{LPARAM, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::commctrl::{
        LVITEMW, LVIS_STATEIMAGEMASK, LVM_GETITEMSTATE, LVM_SETEXTENDEDLISTVIEWSTYLE,
        LVM_SETITEMSTATE, LVM_SETTEXTCOLOR, LVS_EX_CHECKBOXES, LVS_EX_FULLROWSELECT,
    };
    use winapi::um::winuser::{
        GetSysColor, InvalidateRect, SendMessageW, COLOR_GRAYTEXT, COLOR_WINDOWTEXT,
    };

    /// State-image index encodes the checkbox: 1 = unchecked, 2 = checked,
    /// stored in bits 12-15 of the item state.
    fn state_image(checked: bool) -> u32 {
        if checked {
            2 << 12
        } else {
            1 << 12
        }
    }

    pub unsafe fn enable_checkboxes(hwnd: HWND) {
        let style = (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as WPARAM;
        SendMessageW(hwnd, LVM_SETEXTENDEDLISTVIEWSTYLE, style, style as LPARAM);
    }

    pub unsafe fn set_check(hwnd: HWND, row: usize, checked: bool) {
        let mut item: LVITEMW = std::mem::zeroed();
        item.stateMask = LVIS_STATEIMAGEMASK;
        item.state = state_image(checked);
        SendMessageW(
            hwnd,
            LVM_SETITEMSTATE,
            row as WPARAM,
            &item as *const LVITEMW as LPARAM,
        );
    }

    /// Grey out (or restore) the item text to signal the list is disabled.
    /// A disabled report-view ListView custom-draws its rows and so does NOT
    /// grey them on its own, so we drive the text colour explicitly.
    pub unsafe fn set_greyed(hwnd: HWND, greyed: bool) {
        let color = GetSysColor(if greyed { COLOR_GRAYTEXT } else { COLOR_WINDOWTEXT });
        SendMessageW(hwnd, LVM_SETTEXTCOLOR, 0, color as LPARAM);
        InvalidateRect(hwnd, std::ptr::null(), 1);
    }

    pub unsafe fn is_checked(hwnd: HWND, row: usize) -> bool {
        let st = SendMessageW(
            hwnd,
            LVM_GETITEMSTATE,
            row as WPARAM,
            LVIS_STATEIMAGEMASK as LPARAM,
        ) as u32;
        ((st & LVIS_STATEIMAGEMASK) >> 12) == 2
    }
}

/// Windows shell / system UI processes that are never useful capture targets,
/// so we hide them from the picker to reduce clutter.
const SYSTEM_EXES: &[&str] = &[
    "applicationframehost",
    "searchhost",
    "searchapp",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "systemsettings",
    "textinputhost",
    "lockapp",
    "sihost",
    "ctfmon",
    "dwm",
    "runtimebroker",
    "useroobebroker",
    "widgets",
    "widgetservice",
    "explorer",
    "taskmgr",
];

/// A clean, human-friendly label for an app: the product name when available
/// (e.g. "Mozilla Firefox"), with the executable appended only when it adds
/// information. Avoids the noisy raw window titles.
fn friendly_label(product_name: &str, exe: &str) -> String {
    let product = product_name.trim();
    let base = if product.is_empty() {
        exe.to_string()
    } else if product.eq_ignore_ascii_case(exe) {
        product.to_string()
    } else {
        format!("{} ({})", product, exe)
    };
    capitalize_first(&base)
}

/// Upper-cases the first character (e.g. "firefox" -> "Firefox").
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Load the crowd-cast logo (embedded PNG) as an nwg Icon for the window's
/// title-bar / taskbar icon. Returns None if decoding fails (icon is optional).
fn load_logo_icon() -> Option<nwg::Icon> {
    let png: &[u8] = include_bytes!("../../assets/logo.png");
    let img = image::load_from_memory(png).ok()?;
    let small = img.resize_exact(32, 32, image::imageops::FilterType::Lanczos3);
    let mut ico: Vec<u8> = Vec::new();
    small
        .write_to(&mut std::io::Cursor::new(&mut ico), image::ImageFormat::Ico)
        .ok()?;
    let mut icon = nwg::Icon::default();
    nwg::Icon::builder()
        .source_bin(Some(&ico))
        .build(&mut icon)
        .ok()?;
    Some(icon)
}

fn list_windowed_apps() -> Vec<(String, String)> {
    use libobs_simple::sources::windows::{WindowCaptureSourceBuilder, WindowSearchMode};
    use std::collections::BTreeMap;

    // exe stem (lowercase) -> display label
    let mut by_exe: BTreeMap<String, String> = BTreeMap::new();

    if let Ok(windows) = WindowCaptureSourceBuilder::get_windows(WindowSearchMode::ExcludeMinimized) {
        for w in windows {
            let exe = std::path::Path::new(&w.0.full_exe)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if exe.is_empty() {
                continue;
            }
            let exe_l = exe.to_ascii_lowercase();
            if SYSTEM_EXES.contains(&exe_l.as_str()) {
                continue;
            }
            let product = w.0.product_name.clone().unwrap_or_default();
            by_exe
                .entry(exe_l)
                .or_insert_with(|| friendly_label(&product, &exe));
        }
    }

    if by_exe.is_empty() {
        // Fallback: running processes (noisier, but better than an empty list).
        for app in crate::capture::list_capturable_apps() {
            let exe_l = app.bundle_id.to_ascii_lowercase();
            if SYSTEM_EXES.contains(&exe_l.as_str()) {
                continue;
            }
            by_exe.entry(exe_l).or_insert(app.name);
        }
    }

    let mut apps: Vec<(String, String)> = by_exe
        .into_iter()
        .map(|(exe, label)| (label, exe))
        .collect();
    // Sort by the human-friendly label, case-insensitively.
    apps.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    apps
}

/// Raw user selections from the shared app-picker dialog.
#[derive(Clone)]
struct PickerOutcome {
    saved: bool,
    capture_all: bool,
    selected_apps: Vec<String>,
    autostart: bool,
}

/// Build and run the native app-picker dialog on the calling thread, blocking
/// until the user clicks Save / Cancel or closes the window. Shared by the
/// first-run setup wizard and the tray Settings panel. When `show_autostart` is
/// set, the "start at login" row is shown and its state returned; otherwise the
/// row is hidden and `autostart` echoes `autostart_initial` unchanged.
fn run_app_picker(
    title: &str,
    current_apps: &[String],
    capture_all_initial: bool,
    autostart_initial: bool,
    show_autostart: bool,
) -> Result<PickerOutcome> {
    nwg::init().map_err(|e| anyhow::anyhow!("Failed to initialize GUI: {:?}", e))?;

    // The process is Per-Monitor-V2 DPI-aware (set in main, for libobs), but nwg
    // does not scale its controls for DPI. Without help that yields a tiny
    // physical-pixel window rendered with an oversized default font (text looks
    // huge and long labels clip). So scale the window/columns by the system DPI
    // and set an explicit Segoe UI font sized for that DPI.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }.max(96);
    let scale = dpi as f64 / 96.0;
    let px = |design: f64| (design * scale).round() as i32;

    // Native Windows UI is Segoe UI ~9pt; lfHeight in device px = pt * dpi / 72.
    let mut font = nwg::Font::default();
    if nwg::Font::builder()
        .family("Segoe UI")
        .size_absolute((9.0 * dpi as f64 / 72.0).round() as u32)
        .build(&mut font)
        .is_ok()
    {
        nwg::Font::set_global_default(Some(font));
    } else {
        let _ = nwg::Font::set_global_family("Segoe UI");
    }

    let apps = list_windowed_apps();
    let preselect: Vec<usize> = apps
        .iter()
        .enumerate()
        .filter(|(_, (_, exe))| current_apps.iter().any(|t| t.eq_ignore_ascii_case(exe)))
        .map(|(i, _)| i)
        .collect();

    // Held for the lifetime of the window (Windows keeps using the HICON).
    let logo = load_logo_icon();

    let mut window = Default::default();
    let mut intro = Default::default();
    let mut list = Default::default();
    let mut capture_all = Default::default();
    let mut autostart = Default::default();
    let mut save_btn = Default::default();
    let mut cancel_btn = Default::default();
    let layout = Default::default();

    nwg::Window::builder()
        .size((px(460.0), px(520.0)))
        .center(true)
        .icon(logo.as_ref())
        .title(title)
        .build(&mut window)
        .map_err(|e| anyhow::anyhow!("window: {:?}", e))?;

    nwg::Label::builder()
        .text("Check the applications you want to capture:")
        .parent(&window)
        .build(&mut intro)
        .map_err(|e| anyhow::anyhow!("label: {:?}", e))?;

    nwg::ListView::builder()
        .list_style(nwg::ListViewStyle::Detailed)
        .parent(&window)
        .build(&mut list)
        .map_err(|e| anyhow::anyhow!("listview: {:?}", e))?;

    // Single, header-less column that holds the app label + its checkbox.
    list.set_headers_enabled(false);
    list.insert_column(nwg::InsertListViewColumn {
        index: Some(0),
        fmt: None,
        width: Some(px(430.0)),
        text: Some("Application".to_string()),
    });
    for (label, _) in &apps {
        list.insert_item(label.as_str());
    }

    // nwg has no checkbox API, so enable LVS_EX_CHECKBOXES and pre-check the
    // already-configured apps via raw Win32 on the control's HWND.
    if let Some(hwnd) = list.handle.hwnd() {
        unsafe {
            lv::enable_checkboxes(hwnd);
            for &i in &preselect {
                lv::set_check(hwnd, i, true);
            }
        }
    }

    nwg::CheckBox::builder()
        .text("Capture all applications (ignore the checks above)")
        .check_state(check_state(capture_all_initial))
        .parent(&window)
        .build(&mut capture_all)
        .map_err(|e| anyhow::anyhow!("checkbox: {:?}", e))?;

    // The per-app checklist is irrelevant when "capture all" is on, so disable
    // and grey it to match the checkbox's initial state (synced in the handler).
    list.set_enabled(!capture_all_initial);
    if let Some(hwnd) = list.handle.hwnd() {
        unsafe { lv::set_greyed(hwnd, capture_all_initial) };
    }

    // Always created so the event handler can reference it; only added to the
    // layout and read back when `show_autostart` is set. When hidden it would
    // otherwise render at its default (0,0) position over the intro label, so
    // explicitly hide it in that case.
    nwg::CheckBox::builder()
        .text("Start crowd-cast automatically at login")
        .check_state(check_state(autostart_initial))
        .parent(&window)
        .build(&mut autostart)
        .map_err(|e| anyhow::anyhow!("checkbox: {:?}", e))?;
    autostart.set_visible(show_autostart);

    nwg::Button::builder()
        .text("Save")
        .parent(&window)
        .build(&mut save_btn)
        .map_err(|e| anyhow::anyhow!("button: {:?}", e))?;

    nwg::Button::builder()
        .text("Cancel")
        .parent(&window)
        .build(&mut cancel_btn)
        .map_err(|e| anyhow::anyhow!("button: {:?}", e))?;

    let mut lb = nwg::GridLayout::builder()
        .parent(&window)
        .spacing(2)
        .child_item(nwg::GridLayoutItem::new(&intro, 0, 0, 2, 1))
        .child_item(nwg::GridLayoutItem::new(&list, 0, 1, 2, 8))
        .child_item(nwg::GridLayoutItem::new(&capture_all, 0, 9, 2, 1));
    let button_row = if show_autostart {
        lb = lb.child_item(nwg::GridLayoutItem::new(&autostart, 0, 10, 2, 1));
        11
    } else {
        10
    };
    lb.child(0, button_row, &save_btn)
        .child(1, button_row, &cancel_btn)
        .build(&layout)
        .map_err(|e| anyhow::anyhow!("layout: {:?}", e))?;

    let outcome = Rc::new(RefCell::new(PickerOutcome {
        saved: false,
        capture_all: capture_all_initial,
        selected_apps: current_apps.to_vec(),
        autostart: autostart_initial,
    }));
    let apps = Rc::new(apps);
    let window = Rc::new(window);

    let handler = {
        let out = outcome.clone();
        let apps = apps.clone();
        nwg::full_bind_event_handler(&window.handle, move |evt, _data, handle| {
            use nwg::Event as E;
            match evt {
                // Closing the (only) window ends the dialog with saved = false.
                E::OnWindowClose => nwg::stop_thread_dispatch(),
                E::OnButtonClick => {
                    if &handle == &save_btn {
                        let mut selected: Vec<String> = Vec::new();
                        if let Some(hwnd) = list.handle.hwnd() {
                            for (i, (_, exe)) in apps.iter().enumerate() {
                                if unsafe { lv::is_checked(hwnd, i) } {
                                    selected.push(exe.clone());
                                }
                            }
                        }
                        let mut o = out.borrow_mut();
                        o.saved = true;
                        o.selected_apps = selected;
                        o.capture_all = is_checked(&capture_all);
                        o.autostart = is_checked(&autostart);
                        nwg::stop_thread_dispatch();
                    } else if &handle == &cancel_btn {
                        nwg::stop_thread_dispatch();
                    } else if &handle == &capture_all {
                        // Disable + grey the per-app checklist while "capture all" is on.
                        let all = is_checked(&capture_all);
                        list.set_enabled(!all);
                        if let Some(hwnd) = list.handle.hwnd() {
                            unsafe { lv::set_greyed(hwnd, all) };
                        }
                    }
                }
                _ => {}
            }
        })
    };

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);

    let outcome = outcome.borrow().clone();
    Ok(outcome)
}

/// Show the native Windows setup wizard. Blocks until the user clicks Save or
/// Cancel (or closes the window).
pub fn run_wizard_windows(config: &mut Config) -> Result<WizardResult> {
    let outcome = run_app_picker(
        "crowd-cast setup",
        &config.capture.target_apps,
        config.capture.capture_all,
        // Default "start at login" to on for first-time setup; respect the saved
        // value when the user re-runs the wizard later.
        if config.capture.setup_completed {
            config.capture.start_on_login
        } else {
            true
        },
        true,
    )?;

    let mut result = WizardResult::default();
    if !outcome.saved {
        info!("Windows wizard cancelled");
        return Ok(result);
    }

    result.completed = true;
    result.selected_apps = outcome.selected_apps.clone();
    result.capture_all = outcome.capture_all;
    result.autostart_enabled = outcome.autostart;

    info!(
        "Windows wizard completed: {} app(s), capture_all={}, autostart={}",
        result.selected_apps.len(),
        result.capture_all,
        result.autostart_enabled
    );
    config.capture.capture_all = result.capture_all;
    config.capture.target_apps = result.selected_apps.clone();
    config.capture.setup_completed = true;
    config.capture.start_on_login = result.autostart_enabled;

    if result.autostart_enabled {
        if let Err(e) = enable_autostart(&AutostartConfig::default()) {
            info!("Failed to enable autostart: {}", e);
        }
    } else if let Err(e) = disable_autostart() {
        info!("Failed to disable autostart: {}", e);
    }

    config.save()?;
    info!("Configuration saved");

    Ok(result)
}

/// Selection returned by the tray Settings app-picker (no autostart row).
pub struct AppPickerResult {
    pub saved: bool,
    pub capture_all: bool,
    pub selected_apps: Vec<String>,
}

/// Show the app-picker as a standalone Settings panel (invoked from the tray).
/// Reuses the wizard's picker UI without the autostart row or `setup_completed`
/// side effects; the caller persists and applies the returned selection.
pub fn run_settings_panel(current_apps: &[String], capture_all: bool) -> Result<AppPickerResult> {
    let outcome = run_app_picker("crowd-cast settings", current_apps, capture_all, false, false)?;
    info!(
        "Windows settings panel closed: saved={}, {} app(s), capture_all={}",
        outcome.saved,
        outcome.selected_apps.len(),
        outcome.capture_all
    );
    Ok(AppPickerResult {
        saved: outcome.saved,
        capture_all: outcome.capture_all,
        selected_apps: outcome.selected_apps,
    })
}

fn check_state(checked: bool) -> nwg::CheckBoxState {
    if checked {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    }
}

fn is_checked(cb: &nwg::CheckBox) -> bool {
    matches!(cb.check_state(), nwg::CheckBoxState::Checked)
}
