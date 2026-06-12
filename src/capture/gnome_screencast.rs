//! GNOME picker-free per-app capture via the private `org.gnome.Mutter.ScreenCast` API.
//!
//! The public xdg-desktop-portal cannot capture an application without an interactive
//! window picker (and re-prompts unpredictably). On GNOME the only programmatic path is the
//! private Mutter ScreenCast API: `CreateSession` -> `RecordWindow({window-id})` -> `Start`
//! yields a PipeWire node with NO picker and NO consent dialog. The window-id comes from the
//! bundled `crowd-cast-focus` extension's `ListWindows` (an external client cannot enumerate
//! windows itself: `org.gnome.Shell.Introspect.GetWindows` is access-denied). The resulting
//! node is fed to OBS via the obs-pipewire `ConnectNode` setting (see
//! `ScreenCaptureSource::new_window_node_capture` + the bundled-OBS patch).
//!
//! Mutter keeps a stream alive only while its session's D-Bus connection stays open, so a
//! single dedicated thread owns the zbus connection and every session for the capture's
//! lifetime. Dropping [`GnomeScreenCast`] tears the thread down, which closes the sessions.
//!
//! Validated on GNOME 50 / Mutter ScreenCast v4: external `RecordWindow(window-id)` produces
//! a live, consumable PipeWire node with no picker.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use atspi::zbus; // zbus is re-exported by atspi (no direct dependency)
use zbus::zvariant::{OwnedObjectPath, Value};

const SC_DEST: &str = "org.gnome.Mutter.ScreenCast";
const SC_PATH: &str = "/org/gnome/Mutter/ScreenCast";
const SC_IFACE: &str = "org.gnome.Mutter.ScreenCast";
const SESSION_IFACE: &str = "org.gnome.Mutter.ScreenCast.Session";
const STREAM_IFACE: &str = "org.gnome.Mutter.ScreenCast.Stream";

const FP_DEST: &str = "org.crowdcast.FocusProvider";
const FP_PATH: &str = "/org/crowdcast/FocusProvider";
const FP_IFACE: &str = "org.crowdcast.FocusProvider";

/// How long a sync caller waits for the worker thread to answer a command.
const CMD_TIMEOUT: Duration = Duration::from_secs(12);

/// One open toplevel, as reported by the extension's `ListWindows`.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    /// Mutter window stamp (`Meta.Window.get_id()`) — what `RecordWindow` expects.
    pub id: u64,
    pub pid: u32,
    pub wm_class: String,
    pub title: String,
}

enum Cmd {
    ListWindows(mpsc::Sender<Result<Vec<WindowInfo>, String>>),
    /// Record `window_id`; reply with the PipeWire node id. The session is retained (keyed by
    /// window-id) so the node keeps streaming until `StopWindow`/shutdown. Recording a
    /// window-id that already has a live session stops the old one first (no leak).
    RecordWindow(u64, mpsc::Sender<Result<u32, String>>),
    /// Stop and drop the session backing `window_id` (frees the Mutter session + PipeWire
    /// node). Fire-and-forget: used after a focus re-point has bound the new window
    /// (make-before-break), and when an app's window goes away.
    StopWindow(u64),
}

/// Owns the Mutter ScreenCast D-Bus connection + sessions on a dedicated thread.
pub struct GnomeScreenCast {
    tx: mpsc::Sender<Cmd>,
    _thread: std::thread::JoinHandle<()>,
}

impl GnomeScreenCast {
    /// Spawn the worker (connects to the session bus + Mutter ScreenCast lazily). Returns an
    /// error only if the OS thread can't be spawned; D-Bus failures surface per-command.
    pub fn new() -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let thread = std::thread::Builder::new()
            .name("gnome-screencast".into())
            .spawn(move || worker(rx))?;
        Ok(Self { tx, _thread: thread })
    }

    /// All current toplevels (via the focus extension). Empty on any error.
    pub fn list_windows(&self) -> Vec<WindowInfo> {
        let (rtx, rrx) = mpsc::channel();
        if self.tx.send(Cmd::ListWindows(rtx)).is_err() {
            return Vec::new();
        }
        match rrx.recv_timeout(CMD_TIMEOUT) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!("gnome-screencast: ListWindows failed: {e}");
                Vec::new()
            }
            Err(_) => {
                tracing::warn!("gnome-screencast: ListWindows timed out");
                Vec::new()
            }
        }
    }

    /// Record a specific Mutter window-id picker-free, returning its PipeWire node id. The
    /// session is retained (keyed by window-id) so the node keeps streaming until
    /// [`stop_window`](Self::stop_window) or shutdown. Re-recording a still-live window-id
    /// stops the old session first. Fail-closed: `Err` if Mutter declines.
    pub fn record_window(&self, window_id: u64) -> Result<u32, String> {
        let (rtx, rrx) = mpsc::channel();
        self.tx
            .send(Cmd::RecordWindow(window_id, rtx))
            .map_err(|_| "gnome-screencast worker is gone".to_string())?;
        match rrx.recv_timeout(CMD_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err("RecordWindow timed out (Mutter ScreenCast unresponsive)".into()),
        }
    }

    /// Stop and drop the session backing `window_id` (frees its Mutter session + PipeWire
    /// node). Best-effort / fire-and-forget; a no-op if no session is tracked for that id.
    pub fn stop_window(&self, window_id: u64) {
        let _ = self.tx.send(Cmd::StopWindow(window_id));
    }
}

/// Worker thread: owns a current-thread runtime + the zbus connection + every session.
fn worker(rx: mpsc::Receiver<Cmd>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("gnome-screencast: runtime init failed: {e}");
            // Drain commands with errors so callers don't block to timeout.
            for cmd in rx.iter() {
                reply_err(cmd, "runtime init failed");
            }
            return;
        }
    };

    rt.block_on(async move {
        let conn = match zbus::Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("session bus connect failed: {e}");
                tracing::error!("gnome-screencast: {msg}");
                for cmd in rx.iter() {
                    reply_err(cmd, &msg);
                }
                return;
            }
        };

        // Retain session paths (keyed by window-id) so Mutter keeps each stream alive (a
        // session lives as long as this connection stays open and is never Close'd). Closed
        // explicitly on StopWindow (focus re-point) and on shutdown.
        let mut sessions: HashMap<u64, OwnedObjectPath> = HashMap::new();

        // `mpsc::Receiver` is blocking; poll it without starving the zbus executor by handing
        // each blocking recv to a blocking task. (Commands are infrequent: focus switches.)
        loop {
            let cmd = match recv_async(&rx).await {
                Some(c) => c,
                None => break, // all senders dropped → shut down
            };
            match cmd {
                Cmd::ListWindows(reply) => {
                    let _ = reply.send(list_windows(&conn).await);
                }
                Cmd::RecordWindow(id, reply) => {
                    // Re-recording a still-live window-id would leak the old session; stop it
                    // first so each window-id maps to at most one session.
                    if let Some(old) = sessions.remove(&id) {
                        stop_session(&conn, &old).await;
                    }
                    match record_window(&conn, id).await {
                        Ok((node, path)) => {
                            sessions.insert(id, path);
                            let _ = reply.send(Ok(node));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                Cmd::StopWindow(id) => {
                    if let Some(path) = sessions.remove(&id) {
                        stop_session(&conn, &path).await;
                    }
                }
            }
        }

        for path in sessions.values() {
            stop_session(&conn, path).await;
        }
        tracing::debug!("gnome-screencast: worker shut down ({} sessions closed)", sessions.len());
    });
}

/// Await the next blocking-channel command without blocking the async executor.
async fn recv_async(rx: &mpsc::Receiver<Cmd>) -> Option<Cmd> {
    // The receiver isn't Send-safe to move; poll with a short async sleep between tries so the
    // zbus connection's background tasks keep running. Commands are rare, so the latency is
    // irrelevant and CPU cost negligible.
    loop {
        match rx.try_recv() {
            Ok(cmd) => return Some(cmd),
            Err(mpsc::TryRecvError::Empty) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(mpsc::TryRecvError::Disconnected) => return None,
        }
    }
}

fn reply_err(cmd: Cmd, msg: &str) {
    match cmd {
        Cmd::ListWindows(r) => {
            let _ = r.send(Err(msg.to_string()));
        }
        Cmd::RecordWindow(_, r) => {
            let _ = r.send(Err(msg.to_string()));
        }
        Cmd::StopWindow(_) => {}
    }
}

/// Stop a Mutter ScreenCast session (frees its stream/node). Best-effort.
async fn stop_session(conn: &zbus::Connection, path: &OwnedObjectPath) {
    if let Ok(s) = zbus::Proxy::new(conn, SC_DEST, path.clone(), SESSION_IFACE).await {
        let _ = s.call_method("Stop", &()).await;
    }
}

async fn list_windows(conn: &zbus::Connection) -> Result<Vec<WindowInfo>, String> {
    let proxy = zbus::Proxy::new(conn, FP_DEST, FP_PATH, FP_IFACE)
        .await
        .map_err(|e| format!("FocusProvider proxy: {e}"))?;
    let raw: Vec<(u64, i32, String, String)> = proxy
        .call("ListWindows", &())
        .await
        .map_err(|e| format!("ListWindows call: {e} (is the crowd-cast-focus extension loaded?)"))?;
    Ok(raw
        .into_iter()
        .map(|(id, pid, wm_class, title)| WindowInfo {
            id,
            pid: pid.max(0) as u32,
            wm_class,
            title,
        })
        .collect())
}

/// Create a Mutter ScreenCast session recording `window_id` and Start it, returning the
/// PipeWire node id plus the session object path (the caller retains the path to keep the
/// node alive and Stops it later).
async fn record_window(
    conn: &zbus::Connection,
    window_id: u64,
) -> Result<(u32, OwnedObjectPath), String> {
    use futures::StreamExt;

    let sc = zbus::Proxy::new(conn, SC_DEST, SC_PATH, SC_IFACE)
        .await
        .map_err(|e| format!("ScreenCast proxy: {e}"))?;

    let session_path: OwnedObjectPath = sc
        .call("CreateSession", &(HashMap::<&str, Value>::new(),))
        .await
        .map_err(|e| format!("CreateSession: {e}"))?;

    let session = zbus::Proxy::new(conn, SC_DEST, session_path.clone(), SESSION_IFACE)
        .await
        .map_err(|e| format!("Session proxy: {e}"))?;

    let mut props: HashMap<&str, Value> = HashMap::new();
    props.insert("window-id", Value::U64(window_id));
    props.insert("cursor-mode", Value::U32(1)); // embedded cursor
    let stream_path: OwnedObjectPath = session
        .call("RecordWindow", &(props,))
        .await
        .map_err(|e| format!("RecordWindow(window-id={window_id}): {e}"))?;

    let stream = zbus::Proxy::new(conn, SC_DEST, stream_path, STREAM_IFACE)
        .await
        .map_err(|e| format!("Stream proxy: {e}"))?;

    // Subscribe BEFORE Start so the node-id signal can't be missed.
    let mut added = stream
        .receive_signal("PipeWireStreamAdded")
        .await
        .map_err(|e| format!("subscribe PipeWireStreamAdded: {e}"))?;

    session
        .call_method("Start", &())
        .await
        .map_err(|e| format!("Session.Start: {e}"))?;

    let node = match tokio::time::timeout(Duration::from_secs(8), added.next()).await {
        Ok(Some(msg)) => {
            let (node_id,): (u32,) = msg
                .body()
                .deserialize()
                .map_err(|e| format!("PipeWireStreamAdded body: {e}"))?;
            node_id
        }
        Ok(None) => return Err("PipeWireStreamAdded stream ended before a node arrived".into()),
        Err(_) => {
            let _ = session.call_method("Stop", &()).await;
            return Err("timed out waiting for PipeWireStreamAdded".into());
        }
    };

    // Hand the session path back so the worker can retain it (keyed by window-id) and Stop it
    // later. The open connection is what keeps the node alive; the proxy is no longer needed.
    let _ = session;
    Ok((node, session_path))
}
