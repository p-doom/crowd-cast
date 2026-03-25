//! macOS Sparkle updater integration.

use anyhow::{anyhow, Result};
use tracing::{debug, info, warn};

#[cfg(target_os = "macos")]
use crate::ui::current_app_bundle_path;

/// Thin wrapper around the native macOS updater bridge.
#[derive(Debug, Default)]
pub struct UpdaterController {
    available: bool,
    started: bool,
    reason: Option<String>,
}

impl UpdaterController {
    pub fn new() -> Self {
        #[cfg(target_os = "macos")]
        {
            if current_app_bundle_path().is_none() {
                return Self::unavailable("Auto-update is only available from a macOS app bundle.");
            }

            if let Some(bundle_path) = current_app_bundle_path() {
                match is_read_only_volume(&bundle_path) {
                    Ok(true) => {
                        return Self::unavailable(
                            "Auto-update is unavailable from a read-only volume such as a mounted DMG.",
                        );
                    }
                    Ok(false) => {}
                    Err(err) => {
                        warn!("Failed to inspect app volume for updater eligibility: {err}");
                    }
                }
            }

            #[cfg(has_sparkle)]
            {
                return Self {
                    available: true,
                    started: false,
                    reason: None,
                };
            }

            #[cfg(not(has_sparkle))]
            {
                return Self::unavailable(
                    "Sparkle.framework is not bundled in this build. Run scripts/fetch-sparkle.sh before building.",
                );
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Self::unavailable("Auto-update is only implemented on macOS.")
        }
    }

    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            started: false,
            reason: Some(reason.into()),
        }
    }

    pub fn start(&mut self) {
        if !self.available || self.started {
            return;
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        {
            let status = unsafe { ffi::updater_init() };
            if status == 0 {
                self.started = true;
                info!("Sparkle updater initialized");
                self.check_post_update_notification();
            } else {
                let reason = unsafe { ffi::last_error_message() }
                    .unwrap_or_else(|| "Failed to initialize Sparkle updater.".to_string());
                self.available = false;
                self.reason = Some(reason.clone());
                warn!("Sparkle updater unavailable: {reason}");
            }
        }
    }

    /// Check if the app was updated since last launch and show a notification.
    fn check_post_update_notification(&self) {
        let Some(bundle_path) = crate::ui::current_app_bundle_path() else {
            return;
        };
        let plist = bundle_path.join("Contents/Info.plist");

        // Read current version and build from the bundle
        let version = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleShortVersionString", &plist.to_string_lossy()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let build = std::process::Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", "Print :CFBundleVersion", &plist.to_string_lossy()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        if version.is_empty() || build.is_empty() {
            return;
        }

        // Persist last-known build in a small file next to the config
        let marker_path = directories::ProjectDirs::from("dev", "crowd-cast", "agent")
            .map(|p| p.data_dir().join("last_build"))
            .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-last-build"));

        let previous_build = std::fs::read_to_string(&marker_path).unwrap_or_default();
        let previous_build = previous_build.trim();

        // Always write the current build
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&marker_path, &build);

        // Show notification if the build changed (and this isn't the first launch)
        if !previous_build.is_empty() && previous_build != build {
            info!(
                "Detected update: {} -> {} (version {})",
                previous_build, build, version
            );
            super::show_update_completed_notification(&version, &build);
        } else {
            debug!("Build unchanged ({}), no update notification", build);
        }
    }

    pub fn is_available(&self) -> bool {
        self.available && self.started
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub fn can_check_for_updates(&self) -> bool {
        if !self.is_available() {
            return false;
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        unsafe {
            return ffi::updater_can_check_for_updates() != 0;
        }

        #[allow(unreachable_code)]
        false
    }

    pub fn check_for_updates(&self) -> Result<()> {
        if !self.is_available() {
            let reason = self
                .reason()
                .unwrap_or("Auto-update is not available in this build.");
            return Err(anyhow!(reason.to_string()));
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        unsafe {
            if ffi::updater_check_for_updates() == 0 {
                return Ok(());
            }

            let reason = ffi::last_error_message()
                .unwrap_or_else(|| "Failed to trigger Sparkle update check.".to_string());
            return Err(anyhow!(reason));
        }

        #[allow(unreachable_code)]
        Err(anyhow!("Auto-update is not available on this platform."))
    }

    pub fn check_for_updates_in_background(&self) {
        if !self.is_available() {
            return;
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        unsafe {
            ffi::updater_check_for_updates_in_background();
        }
    }

    pub fn take_prepare_for_update_request(&self) -> bool {
        if !self.is_available() {
            return false;
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        unsafe {
            return ffi::updater_take_prepare_for_update_request() != 0;
        }

        #[allow(unreachable_code)]
        false
    }

    pub fn set_busy(&self, busy: bool) {
        if !self.is_available() {
            return;
        }

        #[cfg(all(target_os = "macos", has_sparkle))]
        unsafe {
            ffi::updater_set_busy(if busy { 1 } else { 0 });
        }
    }
}

#[cfg(all(target_os = "macos", has_sparkle))]
mod ffi {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int};

    #[link(name = "updater_darwin", kind = "static")]
    extern "C" {
        pub fn updater_init() -> c_int;
        pub fn updater_can_check_for_updates() -> c_int;
        pub fn updater_check_for_updates() -> c_int;
        pub fn updater_check_for_updates_in_background() -> c_int;
        pub fn updater_take_prepare_for_update_request() -> c_int;
        pub fn updater_set_busy(busy: c_int);
        pub fn updater_last_error_message() -> *const c_char;
    }

    pub unsafe fn last_error_message() -> Option<String> {
        let ptr = updater_last_error_message();
        if ptr.is_null() {
            return None;
        }

        CStr::from_ptr(ptr).to_str().ok().map(|msg| msg.to_string())
    }
}

#[cfg(target_os = "macos")]
fn is_read_only_volume(path: &std::path::Path) -> Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_c = CString::new(path.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    let rc = unsafe { libc::statfs(path_c.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let stat = unsafe { stat.assume_init() };
    Ok((stat.f_flags & libc::MNT_RDONLY as u32) != 0)
}
