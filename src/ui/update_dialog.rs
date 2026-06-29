//! Linux update-check dialog subprocess support.
//!
//! The agent process owns libobs, whose Wayland path runs a GLib loop on the
//! default context. Like the Settings panel, this dialog must render in a clean
//! child process so GTK is never initialized inside the libobs-owning process.

use anyhow::{Context as _, Result};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateDialogStatus {
    pub done: bool,
    pub error: bool,
    pub title: String,
    pub message: String,
}

impl UpdateDialogStatus {
    pub fn checking() -> Self {
        Self {
            done: false,
            error: false,
            title: "Checking for Updates".to_string(),
            message: "CrowdCast is checking for updates.".to_string(),
        }
    }

    pub fn up_to_date(version: &str, build: u64) -> Self {
        Self {
            done: true,
            error: false,
            title: "CrowdCast Is Up to Date".to_string(),
            message: version_build_sentence("Current version", version, build),
        }
    }

    pub fn update_ready(
        version: &str,
        build: u64,
        binary_changed: bool,
        bundle_changed: bool,
        notes: &str,
    ) -> Self {
        let mut parts = vec![
            version_build_sentence("Update", version, build),
            "The update has been downloaded and verified. CrowdCast will restart automatically when recording and uploads are idle.".to_string(),
        ];

        let mut changed = Vec::new();
        if binary_changed {
            changed.push("app");
        }
        if bundle_changed {
            changed.push("OBS runtime");
        }
        if !changed.is_empty() {
            parts.push(format!("Updated components: {}.", changed.join(", ")));
        }

        let notes = notes.trim();
        if !notes.is_empty() {
            parts.push(format!("Release notes:\n{notes}"));
        }

        Self {
            done: true,
            error: false,
            title: "Update Ready to Install".to_string(),
            message: parts.join("\n\n"),
        }
    }

    pub fn failed(error: &str) -> Self {
        Self {
            done: true,
            error: true,
            title: "Update Check Failed".to_string(),
            message: error.to_string(),
        }
    }
}

fn version_build_sentence(prefix: &str, version: &str, build: u64) -> String {
    if build > 0 {
        format!("{prefix}: {version} (build {build}).")
    } else {
        format!("{prefix}: {version}.")
    }
}

pub fn status_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "crowd-cast-update-check-{}.status",
        std::process::id()
    ))
}

pub fn write_status(path: &Path, status: &UpdateDialogStatus) -> Result<()> {
    let body = format!(
        "{}\n{}\n{}\n{}",
        if status.done { "1" } else { "0" },
        if status.error { "1" } else { "0" },
        status.title,
        status.message
    );

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)
        .with_context(|| format!("writing update dialog status to {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("publishing update dialog status to {}", path.display()))?;
    Ok(())
}

pub fn spawn_status_dialog(path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating current executable")?;
    let mut child = Command::new(&exe)
        .arg("--update-check-dialog")
        .arg(path)
        .spawn()
        .with_context(|| {
            format!(
                "spawning update-check dialog subprocess from {}",
                exe.display()
            )
        })?;

    std::thread::spawn(move || wait_for_dialog(&mut child));
    Ok(())
}

fn wait_for_dialog(child: &mut Child) {
    if let Err(e) = child.wait() {
        warn!("Update-check dialog subprocess wait failed: {e}");
    }
}

extern "C" {
    fn show_update_check_dialog(status_path: *const std::os::raw::c_char) -> i32;
}

pub fn run_update_check_dialog_subprocess(path: &Path) -> Result<()> {
    let path_c = CString::new(path.to_string_lossy().as_bytes())
        .context("update dialog status path contains NUL")?;
    let rc = unsafe { show_update_check_dialog(path_c.as_ptr()) };
    if rc == 0 {
        Ok(())
    } else {
        anyhow::bail!("update-check dialog exited with status {rc}")
    }
}

#[cfg(test)]
mod tests {
    use super::UpdateDialogStatus;

    #[test]
    fn formats_build_when_present() {
        let status = UpdateDialogStatus::up_to_date("1.0.3", 42);
        assert!(status.message.contains("1.0.3 (build 42)"));

        let status = UpdateDialogStatus::up_to_date("1.0.3", 0);
        assert_eq!(status.message, "Current version: 1.0.3.");
    }

    #[test]
    fn update_ready_names_changed_artifacts() {
        let status = UpdateDialogStatus::update_ready("1.0.4", 7, true, true, "fixes");
        assert!(status
            .message
            .contains("Updated components: app, OBS runtime."));
        assert!(status.message.contains("Release notes:\nfixes"));
    }
}
