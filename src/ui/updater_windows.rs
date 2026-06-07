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

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use libloading::{Library, Symbol};

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

// WinSparkle C API signatures (see winsparkle.h). wchar_t is u16 on Windows.
type FnVoid = unsafe extern "C" fn();
type FnCStr = unsafe extern "C" fn(*const c_char);
type FnCStrInt = unsafe extern "C" fn(*const c_char) -> c_int;
type FnDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
type FnInt = unsafe extern "C" fn(c_int);
type CanShutdownCb = extern "C" fn() -> c_int;
type ShutdownReqCb = extern "C" fn();
type FnSetCanShutdown = unsafe extern "C" fn(CanShutdownCb);
type FnSetShutdownReq = unsafe extern "C" fn(ShutdownReqCb);

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
    check_update_with_ui: FnVoid,
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
    let check_update_with_ui = sym!(FnVoid, b"win_sparkle_check_update_with_ui\0");
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
        check_update_with_ui,
        check_update_without_ui,
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Load WinSparkle, configure it (appcast, key, version, callbacks, automatic
/// daily checks) and start it. Returns Err (without starting) if the DLL is
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
        (ws.set_automatic_check_for_updates)(1);
        (ws.set_update_check_interval)(60 * 60 * 24); // daily
        (ws.init)();
    }

    let _ = WINSPARKLE.set(ws);
    Ok(())
}

/// Trigger an interactive update check (tray "Check for Updates").
pub fn check_with_ui() {
    if let Some(ws) = WINSPARKLE.get() {
        unsafe { (ws.check_update_with_ui)() };
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
