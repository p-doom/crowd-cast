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
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
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
    let (pid, wm_class, _title): (i32, String, String) =
        proxy.call("GetFocused", &()).await?;
    state.set_live(true);
    publish(state, pid, wm_class);
    tracing::info!("follow-focus(gnome): focus extension connected");

    // Event-driven updates.
    let mut signals = proxy.receive_signal("FocusChanged").await?;
    while let Some(msg) = signals.next().await {
        let (pid, wm_class, _title): (i32, String, String) = msg.body().deserialize()?;
        publish(state, pid, wm_class);
    }
    Ok(())
}

fn publish(state: &Arc<FocusState>, pid: i32, wm_class: String) {
    if pid <= 0 && wm_class.is_empty() {
        state.set(None);
    } else {
        state.set(Some(FocusInfo {
            app_id: wm_class,
            pid: if pid > 0 { Some(pid as u32) } else { None },
        }));
    }
}
