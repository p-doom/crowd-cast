//! Windows: a small, persisted memory of individual WINDOWS (not apps) that stayed bound and
//! dead (never delivered a WGC frame), so the dead-source ladder can tell "this one window
//! never captures" from "the capture stack is wedged" (#137).
//!
//! Field evidence for #137: titled, visible owned dialogs (Ansys Workbench "Mesh Status" /
//! "Solution Status" `#32770` dialogs, Siemens NX "Edge Blend") never deliver WGC frames while
//! the same app's main window captures fine in the same process. The per-app ladder could not
//! see that: it restarted the agent, the fresh process re-bound the same dialog, and the
//! ladder then told the participant to restart their computer.
//!
//! A window's identity is its obs_id at bind time (`title:class:exe`, the encoding of
//! libobs-window-helper), so a new dialog instance with the same title and class (a new HWND)
//! is recognised as the same uncapturable window. Records expire after 24 hours and a record
//! for an identity is cleared the moment that identity is ever seen ready.
//!
//! Stored as `kind<TAB>identity<TAB>unix_secs` lines next to the per-app restart marker. All
//! I/O fails soft: an unreadable file reads as empty (today's ladder), a failed write is
//! ignored.

use std::collections::HashMap;
use std::path::PathBuf;

/// How long a window record is remembered.
pub(crate) const RECORD_TTL_SECS: u64 = 24 * 3600;

/// What we remember about a window identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RecordKind {
    /// The agent restarted for a dead source while this window was bound.
    Restarted,
    /// The window stayed dead after a restart while the capture stack was demonstrably
    /// healthy: it is this window, not the stack.
    Uncapturable,
    /// The "can't record this window" notification was shown for this identity. NOT cleared
    /// when the window becomes ready, so the notification stays at most once per day.
    Notified,
}

impl RecordKind {
    fn tag(self) -> &'static str {
        match self {
            RecordKind::Restarted => "restarted",
            RecordKind::Uncapturable => "uncapturable",
            RecordKind::Notified => "notified",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "restarted" => Some(RecordKind::Restarted),
            "uncapturable" => Some(RecordKind::Uncapturable),
            "notified" => Some(RecordKind::Notified),
            _ => None,
        }
    }
}

/// The identity key for a bound window's obs_id. obs_id already encodes `#` and `:`; only
/// the record-format separators (tab, newline) need neutralising.
pub(crate) fn identity_of(obs_id: &str) -> String {
    obs_id
        .chars()
        .map(|c| {
            if matches!(c, '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// In-memory view of the record file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct WindowRecords {
    entries: HashMap<(RecordKind, String), u64>,
}

impl WindowRecords {
    /// Parse the file body, dropping malformed lines and records older than
    /// [`RECORD_TTL_SECS`] relative to `now`.
    pub(crate) fn parse(body: &str, now: u64) -> Self {
        let mut entries = HashMap::new();
        for line in body.lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(tag), Some(id), Some(ts)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let (Some(kind), Ok(ts)) = (RecordKind::from_tag(tag), ts.trim().parse::<u64>()) else {
                continue;
            };
            if id.is_empty() || now.saturating_sub(ts) > RECORD_TTL_SECS {
                continue;
            }
            entries.insert((kind, id.to_string()), ts);
        }
        Self { entries }
    }

    pub(crate) fn serialize(&self) -> String {
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .map(|((kind, id), ts)| format!("{}\t{}\t{}\n", kind.tag(), id, ts))
            .collect();
        lines.sort();
        lines.concat()
    }

    /// Whether `identity` has an unexpired record of `kind` at `now`.
    pub(crate) fn has(&self, kind: RecordKind, identity: &str, now: u64) -> bool {
        self.entries
            .get(&(kind, identity.to_string()))
            .is_some_and(|ts| now.saturating_sub(*ts) <= RECORD_TTL_SECS)
    }

    /// Record `kind` for `identity` at `now`. Returns whether this added a record that was not
    /// already present (unexpired), so callers only write the file on a real change.
    pub(crate) fn insert(&mut self, kind: RecordKind, identity: &str, now: u64) -> bool {
        if self.has(kind, identity, now) {
            return false;
        }
        self.entries.insert((kind, identity.to_string()), now);
        true
    }

    /// `identity` was seen ready: forget that it was restarted for or judged uncapturable.
    /// The `Notified` record stays (notification de-dupe is per day, not per episode).
    /// Returns whether anything was removed.
    pub(crate) fn clear_ready(&mut self, identity: &str) -> bool {
        let restarted = self
            .entries
            .remove(&(RecordKind::Restarted, identity.to_string()))
            .is_some();
        let uncapturable = self
            .entries
            .remove(&(RecordKind::Uncapturable, identity.to_string()))
            .is_some();
        restarted || uncapturable
    }

    /// Whether any `Restarted`/`Uncapturable` record exists for `identity` (lets the ready
    /// path skip all work in the common case).
    pub(crate) fn tracks(&self, identity: &str) -> bool {
        self.entries
            .contains_key(&(RecordKind::Restarted, identity.to_string()))
            || self
                .entries
                .contains_key(&(RecordKind::Uncapturable, identity.to_string()))
    }
}

fn records_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "crowd-cast", "agent")
        .map(|p| p.data_dir().join("capture_dead_windows"))
}

pub(crate) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the records (empty on any error).
pub(crate) fn load() -> WindowRecords {
    let Some(path) = records_path() else {
        return WindowRecords::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(body) => WindowRecords::parse(&body, unix_now_secs()),
        Err(_) => WindowRecords::default(),
    }
}

/// Persist the records (best effort; removes the file when empty).
pub(crate) fn store(records: &WindowRecords) {
    let Some(path) = records_path() else {
        return;
    };
    if records.entries.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, records.serialize());
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn round_trips_and_drops_expired_and_malformed_lines() {
        let mut r = WindowRecords::default();
        assert!(r.insert(
            RecordKind::Restarted,
            "Mesh Status:#2232770:AnsysWBU.exe",
            NOW
        ));
        assert!(r.insert(
            RecordKind::Uncapturable,
            "Edge Blend:NX_SURFACE_WND_DIALOG:ugraf.exe",
            NOW
        ));
        let body = format!(
            "{}garbage line\nbogus\tid\t5\nuncapturable\told\t{}\nnotified\t\t{}\n",
            r.serialize(),
            NOW - RECORD_TTL_SECS - 1,
            NOW
        );
        let back = WindowRecords::parse(&body, NOW);
        assert_eq!(back, r);
    }

    #[test]
    fn records_expire_after_24h() {
        let mut r = WindowRecords::default();
        r.insert(RecordKind::Uncapturable, "w", NOW);
        assert!(r.has(RecordKind::Uncapturable, "w", NOW + RECORD_TTL_SECS));
        assert!(!r.has(RecordKind::Uncapturable, "w", NOW + RECORD_TTL_SECS + 1));
        // An expired record does not block a fresh insert.
        assert!(r.insert(RecordKind::Uncapturable, "w", NOW + RECORD_TTL_SECS + 1));
        // Expired lines are dropped at load.
        let body = r.serialize();
        assert!(WindowRecords::parse(&body, NOW + 3 * RECORD_TTL_SECS)
            .entries
            .is_empty());
    }

    #[test]
    fn insert_reports_only_real_changes() {
        let mut r = WindowRecords::default();
        assert!(r.insert(RecordKind::Notified, "w", NOW));
        assert!(!r.insert(RecordKind::Notified, "w", NOW + 10));
    }

    #[test]
    fn ready_clears_restart_and_uncapturable_but_keeps_notified() {
        let mut r = WindowRecords::default();
        r.insert(RecordKind::Restarted, "w", NOW);
        r.insert(RecordKind::Uncapturable, "w", NOW);
        r.insert(RecordKind::Notified, "w", NOW);
        r.insert(RecordKind::Uncapturable, "other", NOW);
        assert!(r.tracks("w"));
        assert!(r.clear_ready("w"));
        assert!(!r.tracks("w"));
        assert!(!r.has(RecordKind::Restarted, "w", NOW));
        assert!(!r.has(RecordKind::Uncapturable, "w", NOW));
        assert!(r.has(RecordKind::Notified, "w", NOW));
        assert!(r.has(RecordKind::Uncapturable, "other", NOW));
        assert!(!r.clear_ready("w"));
    }

    #[test]
    fn identity_neutralises_record_separators() {
        assert_eq!(identity_of("a\tb\nc:cls:x.exe"), "a b c:cls:x.exe");
        assert_eq!(
            identity_of("Edge Blend:NX_SURFACE_WND_DIALOG:ugraf.exe"),
            "Edge Blend:NX_SURFACE_WND_DIALOG:ugraf.exe"
        );
    }
}
