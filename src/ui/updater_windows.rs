//! Windows auto-update via WinSparkle.
//!
//! `WinSparkle.dll` is loaded **dynamically** (libloading) rather than linked at
//! load time, so the agent still launches if the DLL is missing — auto-update
//! simply reports unavailable, mirroring how the macOS `UpdaterController`
//! degrades when Sparkle isn't bundled. The installer ships `WinSparkle.dll`
//! next to the exe (and `build.rs` copies it next to the dev exe), so in normal
//! operation it's found right beside the executable.
//!
//! WinSparkle uses the same appcast format + Ed25519 signing as macOS Sparkle,
//! so the release pipeline and key can be shared. The appcast URL and Ed25519
//! public key are baked in at build time (`CROWD_CAST_APPCAST_URL` /
//! `CROWD_CAST_ED_PUBLIC_KEY`); WinSparkle refuses any update not signed by the
//! matching private key.

use std::ffi::{CString, OsString};
use std::os::raw::{c_char, c_int};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use libloading::{Library, Symbol};
use tracing::{error, info};

/// WinSparkle's `int` return for "the callback hit an error" (see winsparkle.h).
const WINSPARKLE_RETURN_ERROR: c_int = -1;

/// Set by WinSparkle (on its own thread) when it has a verified update staged
/// and wants the app to quit so the installer can replace it. Drained by the
/// tray loop via `UpdaterController::take_prepare_for_update_request`, which
/// then runs the same clean-stop path macOS uses (flush segment + stop OBS).
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn can_shutdown_cb() -> c_int {
    // The agent can always shut down cleanly (PrepareForUpdate flushes the
    // current segment and stops OBS), so always allow an update to proceed.
    1
}

extern "C" fn shutdown_request_cb() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

/// Set while a user-initiated check ("Check for Updates") is in flight, so the
/// result callbacks below toast feedback only for manual checks, not for the
/// silent every-10-minute background checks (which would otherwise spam).
static MANUAL_CHECK: AtomicBool = AtomicBool::new(false);

extern "C" fn did_find_update_cb() {
    if MANUAL_CHECK.swap(false, Ordering::SeqCst) {
        crate::ui::notifications::show_update_check_notification(
            "Update available. Downloading and installing it now…",
        );
    }
}

extern "C" fn did_not_find_update_cb() {
    if MANUAL_CHECK.swap(false, Ordering::SeqCst) {
        crate::ui::notifications::show_update_check_notification(
            "You're on the latest version of crowd-cast.",
        );
    }
}

extern "C" fn update_error_cb() {
    if MANUAL_CHECK.swap(false, Ordering::SeqCst) {
        crate::ui::notifications::show_update_check_notification(
            "Couldn't check for updates. Please try again later.",
        );
    }
}

/// Take over launching the staged installer (registered via
/// `win_sparkle_set_user_run_installer_callback`).
///
/// WinSparkle's default launch runs the installer as a child of the agent and
/// *then* asks the agent to quit. On Windows that's fatal: the agent's exit
/// tears the child installer down before it can swap any files, so the update
/// "downloads", the app disappears, and nothing is installed or relaunched.
/// Launching it fully detached (below) lets it outlive our exit, replace the
/// files, and relaunch us via the installer's `[Run] Check: WizardSilent` step.
///
/// Returns `1` ("handled") so WinSparkle skips its own launch and proceeds to
/// request our shutdown; `WINSPARKLE_RETURN_ERROR` on failure (WinSparkle then
/// reports the error and does NOT shut us down).
extern "C" fn run_installer_cb(path: *const u16) -> c_int {
    if path.is_null() {
        error!("WinSparkle run-installer callback received a null path");
        return WINSPARKLE_RETURN_ERROR;
    }
    // SAFETY: WinSparkle passes a valid NUL-terminated wide string (the path to
    // the downloaded, signature-verified installer).
    let len = unsafe { (0..).take_while(|&i| *path.add(i) != 0).count() };
    let wide = unsafe { std::slice::from_raw_parts(path, len) };
    let installer = PathBuf::from(OsString::from_wide(wide));

    match spawn_installer_detached(&installer) {
        Ok(()) => {
            info!(
                "Launched update installer detached: {}",
                installer.display()
            );
            1
        }
        Err(e) => {
            error!(
                "Failed to launch update installer {}: {e}",
                installer.display()
            );
            WINSPARKLE_RETURN_ERROR
        }
    }
}

/// Spawn the Inno installer silently and fully detached, so the agent's
/// imminent exit can't terminate it mid-install. The silent flags mirror the
/// appcast's `sparkle:installerArguments`; the agent self-exits on WinSparkle's
/// shutdown request right after, freeing its files, and the installer's
/// `[Run] Check: WizardSilent` step relaunches the agent once the swap is done.
fn spawn_installer_detached(installer: &Path) -> std::io::Result<()> {
    // Detach from this process's console and group, and break away from any job
    // object, so nothing about our shutdown propagates to the installer.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    let args = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"];
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;

    let spawn = |flags: u32| {
        Command::new(installer)
            .args(args)
            .creation_flags(flags)
            .spawn()
            .map(|_child| ())
    };

    // Prefer breaking away from a job object: a kill-on-close job would
    // otherwise terminate the installer when we exit. If we're not in a job the
    // flag is a no-op; if we're in a job that forbids breakaway the spawn fails
    // with access-denied, so fall back to a plain detached launch.
    match spawn(base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(()) => Ok(()),
        Err(_) => spawn(base),
    }
}

// WinSparkle C API signatures (see winsparkle.h). wchar_t is u16 on Windows.
type FnVoid = unsafe extern "C" fn();
type FnCStr = unsafe extern "C" fn(*const c_char);
type FnCStrInt = unsafe extern "C" fn(*const c_char) -> c_int;
type FnDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
type FnInt = unsafe extern "C" fn(c_int);
type CanShutdownCb = extern "C" fn() -> c_int;
type ShutdownReqCb = extern "C" fn();
type UserRunInstallerCb = extern "C" fn(*const u16) -> c_int;
type FnSetCanShutdown = unsafe extern "C" fn(CanShutdownCb);
type FnSetShutdownReq = unsafe extern "C" fn(ShutdownReqCb);
type FnSetUserRunInstaller = unsafe extern "C" fn(UserRunInstallerCb);
type UpdateStatusCb = extern "C" fn();
type FnSetUpdateStatusCb = unsafe extern "C" fn(UpdateStatusCb);

struct WinSparkle {
    // Kept alive for the process lifetime so the function pointers stay valid.
    _lib: Library,
    init: FnVoid,
    set_appcast_url: FnCStr,
    set_eddsa_public_key: FnCStrInt,
    set_app_details: FnDetails,
    set_automatic_check_for_updates: FnInt,
    set_update_check_interval: FnInt,
    set_can_shutdown_callback: FnSetCanShutdown,
    set_shutdown_request_callback: FnSetShutdownReq,
    set_user_run_installer_callback: FnSetUserRunInstaller,
    // Optional result callbacks (used only for manual-check feedback toasts).
    // Loaded best-effort so an unexpected missing/renamed symbol can never break
    // the core updater (init/check/install); the toast just won't fire.
    set_did_find_update_callback: Option<FnSetUpdateStatusCb>,
    set_did_not_find_update_callback: Option<FnSetUpdateStatusCb>,
    set_update_error_callback: Option<FnSetUpdateStatusCb>,
    check_update_without_ui: FnVoid,
}

// Raw fn pointers + Library are Send/Sync; WinSparkle is only driven from the
// main (tray) thread, which is what it expects.
unsafe impl Send for WinSparkle {}
unsafe impl Sync for WinSparkle {}

static WINSPARKLE: OnceLock<WinSparkle> = OnceLock::new();

fn dll_path() -> Option<PathBuf> {
    // Installed/prod: right next to the executable (the installer ships it there).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("WinSparkle.dll");
            if p.exists() {
                return Some(p);
            }
        }
    }
    // Dev override.
    if let Some(dir) = std::env::var_os("CROWD_CAST_WINSPARKLE_DIR") {
        let p = PathBuf::from(dir).join("WinSparkle.dll");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

unsafe fn load() -> Result<WinSparkle, String> {
    let path =
        dll_path().ok_or_else(|| "WinSparkle.dll not found next to the executable".to_string())?;
    let lib = Library::new(&path).map_err(|e| format!("loading {}: {e}", path.display()))?;

    // Each binding derefs the Symbol to a raw fn pointer inside its own block,
    // so the borrow of `lib` ends before `lib` is moved into the struct below.
    macro_rules! sym {
        ($ty:ty, $name:literal) => {{
            let s: Symbol<$ty> = lib
                .get($name)
                .map_err(|e| format!("{}: {e}", String::from_utf8_lossy($name)))?;
            *s
        }};
    }
    // Best-effort: returns None if the symbol is absent instead of failing load.
    macro_rules! opt_sym {
        ($ty:ty, $name:literal) => {{
            lib.get::<$ty>($name).ok().map(|s| *s)
        }};
    }

    let init = sym!(FnVoid, b"win_sparkle_init\0");
    let set_appcast_url = sym!(FnCStr, b"win_sparkle_set_appcast_url\0");
    let set_eddsa_public_key = sym!(FnCStrInt, b"win_sparkle_set_eddsa_public_key\0");
    let set_app_details = sym!(FnDetails, b"win_sparkle_set_app_details\0");
    let set_automatic_check_for_updates =
        sym!(FnInt, b"win_sparkle_set_automatic_check_for_updates\0");
    let set_update_check_interval = sym!(FnInt, b"win_sparkle_set_update_check_interval\0");
    let set_can_shutdown_callback =
        sym!(FnSetCanShutdown, b"win_sparkle_set_can_shutdown_callback\0");
    let set_shutdown_request_callback =
        sym!(FnSetShutdownReq, b"win_sparkle_set_shutdown_request_callback\0");
    let set_user_run_installer_callback = sym!(
        FnSetUserRunInstaller,
        b"win_sparkle_set_user_run_installer_callback\0"
    );
    let set_did_find_update_callback = opt_sym!(
        FnSetUpdateStatusCb,
        b"win_sparkle_set_did_find_update_callback\0"
    );
    let set_did_not_find_update_callback = opt_sym!(
        FnSetUpdateStatusCb,
        b"win_sparkle_set_did_not_find_update_callback\0"
    );
    let set_update_error_callback =
        opt_sym!(FnSetUpdateStatusCb, b"win_sparkle_set_error_callback\0");
    let check_update_without_ui = sym!(FnVoid, b"win_sparkle_check_update_without_ui\0");

    Ok(WinSparkle {
        _lib: lib,
        init,
        set_appcast_url,
        set_eddsa_public_key,
        set_app_details,
        set_automatic_check_for_updates,
        set_update_check_interval,
        set_can_shutdown_callback,
        set_shutdown_request_callback,
        set_user_run_installer_callback,
        set_did_find_update_callback,
        set_did_not_find_update_callback,
        set_update_error_callback,
        check_update_without_ui,
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Load WinSparkle, configure it (appcast, key, version, callbacks, automatic
/// hourly checks) and start it. Returns Err (without starting) if the DLL is
/// missing or the key is rejected; the caller then marks the updater
/// unavailable. Idempotent.
pub fn init(appcast_url: &str, eddsa_pubkey: &str, version: &str) -> Result<(), String> {
    if WINSPARKLE.get().is_some() {
        return Ok(());
    }

    // Strings are copied internally by WinSparkle, so locals are sufficient.
    let url = CString::new(appcast_url).map_err(|_| "invalid appcast URL".to_string())?;
    let key = CString::new(eddsa_pubkey).map_err(|_| "invalid Ed25519 public key".to_string())?;
    let company = wide("p-doom");
    let app = wide("crowd-cast");
    let ver = wide(version);

    let ws = unsafe { load()? };

    // All configuration must happen before win_sparkle_init().
    unsafe {
        (ws.set_appcast_url)(url.as_ptr());
        if (ws.set_eddsa_public_key)(key.as_ptr()) == 0 {
            return Err("WinSparkle rejected the Ed25519 public key".to_string());
        }
        (ws.set_app_details)(company.as_ptr(), app.as_ptr(), ver.as_ptr());
        (ws.set_can_shutdown_callback)(can_shutdown_cb);
        (ws.set_shutdown_request_callback)(shutdown_request_cb);
        // Launch the installer ourselves (detached) instead of letting WinSparkle
        // run it as a child that dies with us. See run_installer_cb.
        (ws.set_user_run_installer_callback)(run_installer_cb);
        // Result callbacks for manual-check feedback toasts (best-effort).
        if let Some(f) = ws.set_did_find_update_callback {
            f(did_find_update_cb);
        }
        if let Some(f) = ws.set_did_not_find_update_callback {
            f(did_not_find_update_cb);
        }
        if let Some(f) = ws.set_update_error_callback {
            f(update_error_cb);
        }
        (ws.set_automatic_check_for_updates)(1);
        // Check hourly (WinSparkle's enforced minimum) rather than daily, so a
        // freshly published release reaches users within ~an hour instead of up
        // to a day. The background check respects this interval, so a long-running
        // agent would otherwise sit on a stale version until the daily mark.
        (ws.set_update_check_interval)(60 * 60);
        (ws.init)();
    }

    let _ = WINSPARKLE.set(ws);
    Ok(())
}

/// Trigger a manual update check (tray "Check for Updates").
///
/// WinSparkle's interactive (with-UI) check does not render a dialog for our
/// windowless tray app (the click reaches it, but no window appears). So drive
/// the same silent check the background loop uses, which reliably finds,
/// downloads, and installs updates, and report progress/result via toasts from
/// the did-find / did-not-find / error callbacks (gated on MANUAL_CHECK so the
/// every-10-minute background checks stay silent).
pub fn check_with_ui() {
    if let Some(ws) = WINSPARKLE.get() {
        info!("Manual update check requested (tray Check for Updates)");
        MANUAL_CHECK.store(true, Ordering::SeqCst);
        crate::ui::notifications::show_update_check_notification("Checking for updates…");
        unsafe { (ws.check_update_without_ui)() };
    } else {
        error!("Manual update check requested but WinSparkle is not initialized");
    }
}

/// Trigger a silent background update check.
pub fn check_without_ui() {
    if let Some(ws) = WINSPARKLE.get() {
        unsafe { (ws.check_update_without_ui)() };
    }
}

/// Returns true once (and resets) if WinSparkle has asked the app to quit for
/// an install.
pub fn take_shutdown_request() -> bool {
    SHUTDOWN_REQUESTED.swap(false, Ordering::SeqCst)
}
