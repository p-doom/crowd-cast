//! GNOME follow-focus provider: consumes the bundled `crowd-cast-focus` extension's
//! private D-Bus interface (`org.crowdcast.FocusProvider`). The extension reports the
//! focused window's (pid, wm_class, title); we use wm_class as the identity and carry pid.
//!
//! Liveness doubles as name-watching: until gnome-shell has loaded the extension (i.e.
//! before the post-install relogin), the initial `GetFocused` call fails and we retry, so
//! the provider goes live on its own once the extension appears — no crowd-cast restart.
//!
//! NOTE: this path is authored against the zbus API and compiles, but is not exercised on
//! this (sway) machine; it needs validation on a GNOME Wayland session.

use std::sync::Arc;
use std::time::Duration;

use crate::installer::gnome_focus::BUS_NAME;
// zbus is not a direct dependency; it is re-exported by the `atspi` crate (see Cargo.toml).
use atspi::zbus;

use super::{FocusInfo, FocusState};

const OBJ_PATH: &str = "/org/crowdcast/FocusProvider";
const IFACE: &str = "org.crowdcast.FocusProvider";

pub fn spawn(state: Arc<FocusState>) {
    std::thread::Builder::new()
        .name("focus-gnome".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("follow-focus(gnome): runtime init failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                loop {
                    if let Err(e) = run_once(&state).await {
                        tracing::debug!("follow-focus(gnome): {e}");
                    }
                    // Lost the extension (not yet loaded / disabled / shell restart). Gate
                    // off and retry — this is the "name-watching": the provider goes live
                    // on its own once the extension appears (e.g. after the relogin).
                    state.set_live(false);
                    state.set(None);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        })
        .ok();
}

async fn run_once(state: &Arc<FocusState>) -> zbus::Result<()> {
    use futures::StreamExt;

    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&conn, BUS_NAME, OBJ_PATH, IFACE).await?;

    // Initial fetch — also confirms the extension owns the name (else this errors → retry).
    // (window_id, pid, wm_class, title); title is unused now (window_id pins the window).
    let (window_id, pid, wm_class, _title): (u64, i32, String, String) =
        proxy.call("GetFocused", &()).await?;
    state.set_live(true);
    publish(state, window_id, pid, wm_class);
    tracing::info!("follow-focus(gnome): focus extension connected");

    // Event-driven updates.
    let mut signals = proxy.receive_signal("FocusChanged").await?;
    while let Some(msg) = signals.next().await {
        let (window_id, pid, wm_class, _title): (u64, i32, String, String) =
            msg.body().deserialize()?;
        publish(state, window_id, pid, wm_class);
    }
    Ok(())
}

/// One-shot enumeration of all open windows' `wm_class` via the extension's `ListWindows`.
/// This is the SAME `get_wm_class()` value the focus signal (and thus the gate) reports, so
/// the wizard's app list and runtime gating agree by construction — no `.desktop`/wm_class
/// heuristic. Returns an empty vec if the extension isn't live (fail closed; the extension is
/// a hard recording prerequisite anyway). Runs the async zbus call on a fresh thread so it is
/// safe to call from either a sync context or inside an existing tokio runtime.
pub(super) fn list_app_ids() -> Vec<String> {
    std::thread::Builder::new()
        .name("focus-gnome-list".into())
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async {
                let conn = zbus::Connection::session().await.ok()?;
                let proxy = zbus::Proxy::new(&conn, BUS_NAME, OBJ_PATH, IFACE)
                    .await
                    .ok()?;
                // ListWindows -> a(tiss): (window_id, pid, wm_class, title) per window.
                let windows: Vec<(u64, i32, String, String)> =
                    proxy.call("ListWindows", &()).await.ok()?;
                Some(
                    windows
                        .into_iter()
                        .map(|(_, _, cls, _)| cls)
                        .collect::<Vec<_>>(),
                )
            })
        })
        .ok()
        .and_then(|h| h.join().ok())
        .flatten()
        .unwrap_or_default()
}

fn publish(state: &Arc<FocusState>, window_id: u64, pid: i32, wm_class: String) {
    if pid <= 0 && wm_class.is_empty() {
        state.set(None);
    } else {
        state.set(Some(FocusInfo {
            app_id: wm_class,
            pid: if pid > 0 { Some(pid as u32) } else { None },
            window_id: if window_id > 0 { Some(window_id) } else { None },
        }));
    }
}
