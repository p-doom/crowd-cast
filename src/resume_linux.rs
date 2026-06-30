//! Linux resume-from-suspend listener.
//!
//! Subscribes to logind's `org.freedesktop.login1.Manager.PrepareForSleep` signal on the **system**
//! bus. That signal carries a bool: `true` just before the system suspends, `false` right after it
//! resumes. On the resume edge we ask the engine to restart the recording fresh (so keylog and video
//! re-zero together) — the duration-independent counterpart to macOS's restart-on-unlock path.
//!
//! This is the *primary* resume signal; the engine's wall-clock-gap check (see `sync::engine`) is the
//! fallback for environments without logind. Reuses the same zbus idiom as `capture::gnome_screencast`.

use atspi::zbus;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::sync::EngineCommand;

const LOGIND_DEST: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER_IFACE: &str = "org.freedesktop.login1.Manager";

/// Listen for resume-from-suspend until the engine command channel closes. Reconnects on a dropped
/// signal stream or a transient D-Bus error so a single bus hiccup can't silently disable resume
/// handling for the rest of the run. Spawned on the engine runtime by `main`.
pub async fn run(cmd_tx: mpsc::Sender<EngineCommand>) {
    loop {
        if cmd_tx.is_closed() {
            return;
        }
        match listen(&cmd_tx).await {
            Ok(()) => debug!("resume listener: signal stream ended; reconnecting in 5s"),
            Err(e) => warn!("resume listener: {e}; reconnecting in 5s"),
        }
        if cmd_tx.is_closed() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn listen(cmd_tx: &mpsc::Sender<EngineCommand>) -> zbus::Result<()> {
    let conn = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(&conn, LOGIND_DEST, LOGIND_PATH, LOGIND_MANAGER_IFACE).await?;
    let mut signals = proxy.receive_signal("PrepareForSleep").await?;
    info!("Subscribed to logind PrepareForSleep (resume-from-suspend handling)");

    while let Some(msg) = signals.next().await {
        // PrepareForSleep(b): true = about to sleep, false = just resumed.
        let (sleeping,): (bool,) = match msg.body().deserialize() {
            Ok(v) => v,
            Err(e) => {
                debug!("PrepareForSleep body decode failed: {e}");
                continue;
            }
        };
        if sleeping {
            debug!("logind: system about to sleep");
            continue;
        }
        info!("logind: system resumed — requesting fresh recording");
        if cmd_tx.try_send(EngineCommand::ResumeFromSuspend).is_err() {
            if cmd_tx.is_closed() {
                return Ok(());
            }
            // Channel momentarily full; the engine's wall-clock-gap check still catches the resume.
            warn!("resume listener: command channel full; relying on gap fallback");
        }
    }
    Ok(())
}
