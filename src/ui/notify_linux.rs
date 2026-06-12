//! Minimal Linux desktop notifications via the `org.freedesktop.Notifications` D-Bus service.
//! The cross-platform `show_*` functions in `notifications.rs` route here on Linux (other
//! platforms get no-op stubs), so the same notification surface macOS gets via
//! UNUserNotificationCenter is mirrored on Linux. Best-effort: errors are logged and ignored.
//!
//! Uses zbus (re-exported via the `atspi` crate; no new dependency, no external binary).
//! Requires a running notification daemon (GNOME/KDE provide one; on bare wlroots the user
//! needs e.g. mako/dunst) — `service_available()` reports whether one is present.
//!
//! Delivery waits for the D-Bus call to flush (bounded by a timeout) rather than firing and
//! forgetting: the agent restarts often (start, record, shut down, relaunch), and a detached
//! send would be killed by process exit before the message reached the daemon, which is why
//! notifications appeared to silently never show.
#![cfg(target_os = "linux")]

use atspi::zbus;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::warn;

/// How long the synchronous senders wait for a notification to flush before giving up. The call
/// normally completes in a few milliseconds; the cap only guards against a wedged daemon so a
/// notification can never block the engine for long.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Caches a positive daemon probe so the common case costs one bus round-trip per process, not
/// one per notification. Only ever flips false->true: a daemon that later dies just makes
/// notifications best-effort no-ops, which is already how this module behaves.
static SERVICE_SEEN: AtomicBool = AtomicBool::new(false);

/// Show a desktop notification (best-effort, async). Used directly by the async engine paths,
/// which `.await` it so the send completes before they move on.
pub async fn notify(summary: &str, body: &str) {
    if let Err(e) = try_notify(summary, body).await {
        // WARN, not DEBUG: the default log filter is `info`, so a debug line would be invisible
        // and a silently-failing notification would be undiagnosable.
        warn!("desktop notification failed (is a notification daemon running?): {e}");
    }
}

/// Synchronous notification for the (sync) `notifications.rs` facade and the portal-picker cue.
/// Runs the async send on a dedicated thread with its own current-thread runtime (so it never
/// nests inside a caller's runtime) and WAITS for it to flush, up to `DELIVERY_TIMEOUT`. Waiting
/// is what makes it survive the agent's frequent restarts: by the time this returns, the D-Bus
/// message has been handed to the daemon, so an imminent process exit can't drop it.
pub fn notify_blocking(summary: &str, body: &str) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (summary, body) = (summary.to_string(), body.to_string());
    let spawned = std::thread::Builder::new()
        .name("cc-notify".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(notify(&summary, &body)),
                Err(e) => warn!("notify runtime build failed: {e}"),
            }
            let _ = tx.send(());
        });
    if spawned.is_err() {
        warn!("failed to spawn desktop-notification thread");
        return;
    }
    // If the daemon wedges, stop waiting after the timeout; the worker thread is left to finish
    // (or die with the process). The common path completes in well under this.
    if rx.recv_timeout(DELIVERY_TIMEOUT).is_err() {
        warn!("desktop notification did not complete within {DELIVERY_TIMEOUT:?}");
    }
}

/// Whether a notification daemon currently owns `org.freedesktop.Notifications` on the session
/// bus. This is the Linux analog of macOS notification authorization: if false, there is nobody
/// to display a notification, so callers should not bother. Blocking (a quick bus query); a
/// positive result is cached for the process lifetime.
pub fn service_available() -> bool {
    if SERVICE_SEEN.load(Ordering::Relaxed) {
        return true;
    }
    let available = std::thread::Builder::new()
        .name("cc-notify-probe".into())
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map(|rt| rt.block_on(probe_service()))
                .unwrap_or(false)
        })
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or(false);
    if available {
        SERVICE_SEEN.store(true, Ordering::Relaxed);
    }
    available
}

async fn probe_service() -> bool {
    let Ok(conn) = zbus::Connection::session().await else {
        return false;
    };
    let proxy = match zbus::Proxy::new(
        &conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    {
        Ok(p) => p,
        Err(_) => return false,
    };
    proxy
        .call("NameHasOwner", &("org.freedesktop.Notifications",))
        .await
        .unwrap_or(false)
}

async fn try_notify(summary: &str, body: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await?;
    let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
    // Notify(app_name s, replaces_id u, app_icon s, summary s, body s,
    //        actions as, hints a{sv}, expire_timeout i) -> id u
    let _id: u32 = proxy
        .call(
            "Notify",
            &(
                "crowd-cast",
                0u32,
                "",
                summary,
                body,
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await?;
    Ok(())
}
