//! Best-effort background shipping of app log files to S3, so participant
//! issues can be debugged remotely without asking them to dig out their
//! local log directory.
//!
//! Logs land under a `logs/` sub-prefix beside the recording uploads
//! (`uploads/<version>/<user>/logs/`). The metadata pipeline ignores them —
//! it only matches `recording_*.mp4` / `input_*.msgpack` keys.
//!
//! Shipping rule: a file whose on-disk size differs from the size recorded
//! at its last successful upload is re-uploaded in full, overwriting the
//! same S3 key. That one rule covers all three file kinds:
//! - the live daily log re-ships as it grows (same-day remote debugging,
//!   at most one tick of lag),
//! - a rotated daily ships one final time after midnight and then never
//!   again,
//! - the cumulative crash.log re-ships whenever a new crash appends to it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tracing::{debug, warn};

use super::Uploader;

/// Only ship files touched within this window. Bounds the first-rollout
/// backfill and skips stale files from long-dead installs.
const MAX_LOG_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Map a local log file name to its S3 name under `logs/`, or `None` for
/// files we don't ship. Rotated dailies are renamed
/// `crowd-cast.log.YYYY-MM-DD` -> `crowd-cast-YYYY-MM-DD.log` because the
/// upload backend only accepts a fixed set of file extensions.
fn remote_log_name(local_name: &str) -> Option<String> {
    if local_name == "crash.log" {
        return Some(local_name.to_string());
    }
    let date = local_name.strip_prefix("crowd-cast.log.")?;
    if date.is_empty() {
        return None;
    }
    Some(format!("crowd-cast-{}.log", date))
}

pub struct LogShipper {
    uploader: Uploader,
    log_dir: PathBuf,
    state_path: PathBuf,
}

impl LogShipper {
    /// Returns `None` when the log or data directory can't be resolved
    /// (nothing to ship / nowhere to record state).
    pub fn new(uploader: Uploader) -> Option<Self> {
        let log_dir = crate::logging::get_log_dir().ok()?;
        let state_path = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .map(|p| p.data_dir().join("logs_shipped.json"))?;
        Some(Self {
            uploader,
            log_dir,
            state_path,
        })
    }

    fn read_state(&self) -> HashMap<String, u64> {
        std::fs::read_to_string(&self.state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn write_state(&self, state: &HashMap<String, u64>) {
        if let Some(parent) = self.state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(state) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.state_path, json) {
                    warn!("Failed to persist log-ship state: {}", e);
                }
            }
            Err(e) => warn!("Failed to serialize log-ship state: {}", e),
        }
    }

    /// One shipping pass: upload every log file whose size changed since its
    /// last successful upload. Best-effort — failures are logged and retried
    /// on the next pass (the recorded size stays stale until success).
    pub async fn run_once(&self) {
        let entries = match std::fs::read_dir(&self.log_dir) {
            Ok(entries) => entries,
            Err(e) => {
                debug!("Log dir not readable, skipping log shipping: {}", e);
                return;
            }
        };

        let mut state = self.read_state();
        let mut shipped = 0u32;
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();

        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            present.insert(file_name.clone());
            let Some(remote_name) = remote_log_name(&file_name) else {
                continue;
            };
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() == 0 {
                continue;
            }
            let recently_touched = metadata
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age < MAX_LOG_AGE);
            if !recently_touched {
                continue;
            }
            if state.get(&file_name) == Some(&metadata.len()) {
                continue;
            }

            match self
                .uploader
                .upload_log_file(&entry.path(), &remote_name)
                .await
            {
                Ok(uploaded_len) => {
                    // Record the byte count actually read for upload — if the
                    // live file grew mid-upload, the size mismatch re-ships it
                    // next pass.
                    state.insert(file_name, uploaded_len);
                    self.write_state(&state);
                    shipped += 1;
                }
                Err(e) => {
                    warn!("Log shipping failed for {}: {:#}", remote_name, e);
                }
            }
        }

        // Drop state for files the local 7-day log pruning has deleted, so
        // the state file doesn't accumulate dead entries forever.
        let before = state.len();
        state.retain(|name, _| present.contains(name));
        if state.len() != before {
            self.write_state(&state);
        }

        // debug!, deliberately NOT info!: an info line here would be written
        // to the very log file we ship, growing it every pass — so the next
        // pass would see a size change and re-upload forever, even on a
        // fully idle machine. At debug level an idle app's log stops
        // growing and shipping quiesces; S3 itself is the evidence that
        // shipping works.
        if shipped > 0 {
            debug!("Shipped {} log file(s) to S3", shipped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::remote_log_name;

    #[test]
    fn rotated_daily_gets_log_extension() {
        assert_eq!(
            remote_log_name("crowd-cast.log.2026-07-22").as_deref(),
            Some("crowd-cast-2026-07-22.log")
        );
    }

    #[test]
    fn crash_log_ships_as_is() {
        assert_eq!(remote_log_name("crash.log").as_deref(), Some("crash.log"));
    }

    #[test]
    fn unrelated_files_are_skipped() {
        assert_eq!(remote_log_name("perf-sampler.csv"), None);
        assert_eq!(remote_log_name("crowd-cast.log."), None);
        assert_eq!(remote_log_name("notes.txt"), None);
    }
}
