//! Minimal Linux desktop notifications via the `org.freedesktop.Notifications` D-Bus service.
//! The cross-platform `show_*` functions in `notifications.rs` route here on Linux (other
//! platforms get no-op stubs), so the same notification surface macOS gets via
//! UNUserNotificationCenter is mirrored on Linux. Best-effort: errors are logged and ignored.
//!
//! Uses zbus (re-exported via the `atspi` crate; no new dependency, no external binary).
//! Requires a running notification daemon (GNOME/KDE provide one; on bare wlroots the user
//! needs e.g. mako/dunst) — `service_available()` reports whether one is present.
//!
//! ## One long-lived connection (do NOT make this per-notification)
//! GNOME Shell's notification server destroys a notification the instant the *sending D-Bus
//! connection* disconnects — but only when the notification resolved to an installed application
//! (`FdoNotificationDaemonSource._onNameVanished`: `if (this.app) this.destroy()`; the carve-out
//! exists so short-lived `notify-send` invocations don't linger). Our notifications carry
//! `app_name = "crowd-cast"`, which the shell resolves to the installed `crowd-cast.desktop`, so a
//! throwaway connection per send (open → Notify → drop) gets the notification torn down within
//! milliseconds, before it is ever shown. We therefore send every notification over ONE connection
//! that lives for the whole process (owned by a dedicated notifier thread): its bus name never
//! vanishes while the agent runs, so the shell leaves the notification alone and it expires
//! normally (see `EXPIRE_TIMEOUT_MS`). Confirmed against GNOME Shell 50.1 by reading the running
//! `_onNameVanished` source and reproducing: a sender that holds its connection open displays and
//! keeps the toast; a throwaway sender's toast is destroyed on disconnect.
//!
//! Edge case: a notification emitted in the last few seconds before the agent itself exits can
//! still be torn down when the process — and thus this connection — goes away. Acceptable: the
//! common case (start/stop/etc. while the agent keeps running) now works, which it never did.
#![cfg(target_os = "linux")]

use atspi::zbus;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::OnceLock;
use std::time::Duration;
use tracing::warn;

/// How long the synchronous senders wait for a notification to flush before giving up. The call
/// normally completes in a few milliseconds; the cap only guards against a wedged daemon so a
/// notification can never block the engine for long.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the daemon should display each notification before auto-dismissing it, in
/// milliseconds. We pass this explicitly rather than the spec's `-1` ("use the server's default
/// policy") because that default is daemon-specific and not always "auto-dismiss": GNOME Shell
/// expires `-1` notifications after a few seconds, but mako's `default-timeout` is `0` (never
/// expire), so on bare wlroots every crowd-cast notification would pile up and stay on screen
/// forever. All of our notifications are transient, informational banners (the macOS side shows
/// them via UNUserNotificationCenter, which auto-dismisses too), so we own the lifetime here and
/// give every daemon the same behavior. 5s mirrors the typical banner duration.
const EXPIRE_TIMEOUT_MS: i32 = 5000;

/// Caches a positive daemon probe so the common case costs one bus round-trip per process, not
/// one per notification. Only ever flips false->true: a daemon that later dies just makes
/// notifications best-effort no-ops, which is already how this module behaves.
static SERVICE_SEEN: AtomicBool = AtomicBool::new(false);

/// A notification handed to the notifier thread, plus a one-shot ack the caller waits on so it
/// knows the D-Bus call has flushed (preserving the "delivered before process exit" guarantee).
struct NotifyRequest {
    summary: String,
    body: String,
    done: SyncSender<()>,
}

/// Channel to the long-lived notifier thread, started on first use. The thread owns the single
/// persistent `zbus::Connection` (see the module docs) for the life of the process, so the sender
/// bus name never vanishes mid-run.
static NOTIFIER: OnceLock<Sender<NotifyRequest>> = OnceLock::new();

fn notifier() -> &'static Sender<NotifyRequest> {
    NOTIFIER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<NotifyRequest>();
        if std::thread::Builder::new()
            .name("cc-notify".into())
            .spawn(move || notifier_loop(rx))
            .is_err()
        {
            warn!("failed to spawn desktop-notification thread");
        }
        tx
    })
}

/// Owns the persistent connection and services notification requests one at a time for the life of
/// the process. Reusing the same connection is the whole point: its bus name stays registered, so
/// GNOME Shell does not tear our notifications down on sender disconnect (see module docs). The
/// connection object being held open keeps the socket — and thus the name — alive even between
/// sends; the runtime only needs to be driven (via `block_on`) for the duration of each call.
fn notifier_loop(rx: Receiver<NotifyRequest>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!("notify runtime build failed: {e}");
            drain(rx);
            return;
        }
    };

    let conn = match rt.block_on(zbus::Connection::session()) {
        Ok(c) => c,
        Err(e) => {
            warn!("desktop notifications unavailable (cannot open session bus): {e}");
            drain(rx);
            return;
        }
    };

    for req in rx {
        rt.block_on(async {
            if let Err(e) = send(&conn, &req.summary, &req.body).await {
                // WARN, not DEBUG: the default log filter is `info`, so a debug line would be
                // invisible and a silently-failing notification would be undiagnosable.
                warn!("desktop notification failed (is a notification daemon running?): {e}");
            }
        });
        let _ = req.done.send(());
    }
}

/// Ack every pending request without sending, so callers waiting on the one-shot don't block for
/// the full `DELIVERY_TIMEOUT` when the notifier couldn't start (no runtime / no bus).
fn drain(rx: Receiver<NotifyRequest>) {
    for req in rx {
        let _ = req.done.send(());
    }
}

/// Show a desktop notification (best-effort, async). Used by the async engine paths, which
/// `.await` it so the send has flushed before they move on. Defers to the blocking submit on
/// tokio's blocking pool so it never stalls the caller's async workers.
pub async fn notify(summary: &str, body: &str) {
    let (summary, body) = (summary.to_string(), body.to_string());
    let _ = tokio::task::spawn_blocking(move || notify_blocking(&summary, &body)).await;
}

/// Synchronous notification for the (sync) `notifications.rs` facade and the portal-picker cue.
/// Hands the request to the long-lived notifier thread and WAITS (up to `DELIVERY_TIMEOUT`) for it
/// to flush, so an imminent process exit can't drop an in-flight send.
pub fn notify_blocking(summary: &str, body: &str) {
    let (done, ack) = sync_channel::<()>(1);
    let req = NotifyRequest {
        summary: summary.to_string(),
        body: body.to_string(),
        done,
    };
    if notifier().send(req).is_err() {
        warn!("desktop notification dropped: notifier thread is not running");
        return;
    }
    // If the daemon wedges, stop waiting after the timeout; the notifier thread keeps the request
    // (or dies with the process). The common path completes in well under this.
    if ack.recv_timeout(DELIVERY_TIMEOUT).is_err() {
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

/// Post one notification over the shared, long-lived connection.
async fn send(conn: &zbus::Connection, summary: &str, body: &str) -> zbus::Result<()> {
    let proxy = zbus::Proxy::new(
        conn,
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
                EXPIRE_TIMEOUT_MS,
            ),
        )
        .await?;
    Ok(())
}
