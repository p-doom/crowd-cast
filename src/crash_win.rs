//! Windows-only native crash & session diagnostics (PDOOM-1448).
//!
//! On Windows the release binary runs under the GUI subsystem
//! (`windows_subsystem = "windows"`, see main.rs), so anything OBS prints to stderr
//! (including its formatted native-crash report) is discarded, and `crash.log` only
//! ever captured Rust panics. A native fault therefore left no trace and was
//! indistinguishable from a clean quit, a kill, or a logoff.
//!
//! This module closes that gap with three Windows-specific pieces. The cross-platform
//! "previous run did not exit cleanly" sentinel (part 3) lives in `crash.rs`.
//!
//!  1. `install_native_crash_handlers` replaces libobs-wrapper's default
//!     `ConsoleCrashHandler` (whose `eprintln!` goes to the discarded stderr) with one
//!     that appends OBS's own crash report to `crash.log` (the file the log shipper
//!     uploads), fsyncs it, logs one error, and exits with a distinctive code. It also
//!     installs a fallback top-level unhandled-exception filter that records the
//!     exception code, faulting address and module into `crash.log` before chaining to
//!     OBS's filter.
//!  2. `watch_session_end` owns a hidden top-level window so we receive
//!     `WM_QUERYENDSESSION`/`WM_ENDSESSION` and log a logoff/shutdown, so a session end
//!     stops looking like a native crash (the console-control handler in main.rs does
//!     NOT get these for a GUI-subsystem process).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

use tracing::{error, info, warn};

/// Process exit code used when a native OBS crash is caught (part 1). Distinct from the
/// clean-exit `0` and the generic unexpected-exit `1` that main() uses, so a native
/// fault is identifiable post-mortem.
pub const OBS_CRASH_EXIT_CODE: i32 = 0xC0B5; // 49333

/// Our replacement for libobs-wrapper's default `ConsoleCrashHandler`, whose `eprintln!`
/// lands on the discarded stderr of a GUI-subsystem process.
struct FileCrashHandler;

impl libobs_wrapper::crash_handler::ObsCrashHandler for FileCrashHandler {
    fn handle_crash(&self, message: String) {
        let sep = "=".repeat(80);
        let report = format!(
            "\n{sep}\nOBS NATIVE CRASH at {ts}\n{sep}\n{message}\n{sep}\n",
            ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        );
        crate::crash::append_to_crash_log(&report);
        error!("OBS native crash captured to crash.log; exiting with code {OBS_CRASH_EXIT_CODE}");
        // The report is already fsynced to crash.log; exit with a distinctive code so a
        // native fault is not mistaken for a clean or generic-unexpected exit.
        std::process::exit(OBS_CRASH_EXIT_CODE);
    }
}

/// The top-level exception filter installed before ours (OBS's), stored as a raw
/// pointer so our filter can chain to it. `0` means none.
static PREV_EXCEPTION_FILTER: AtomicUsize = AtomicUsize::new(0);

/// Guards one-time installation of parts 1 and 2. `initialize()` runs on every OBS
/// (re)init, so this keeps us from chaining our own filter onto itself.
static INSTALL: Once = Once::new();

/// Install parts 1 and 2. Call right after `ObsContext::new()`. Safe to call on every
/// OBS (re)initialization; it takes effect exactly once.
pub fn install_native_crash_handlers() {
    INSTALL.call_once(|| {
        // Part 1: own the OBS crash report. libobs-wrapper already routes OBS's Win32
        // exception filter through this global; we only swap in our handler.
        match libobs_wrapper::crash_handler::CRASH_HANDLER.lock() {
            Ok(mut handler) => {
                *handler = Box::new(FileCrashHandler);
                info!("Installed OBS crash handler (report -> crash.log)");
            }
            Err(e) => warn!("Could not install OBS crash handler: {e}"),
        }

        // Part 2: fallback top-level unhandled-exception filter. Installing after OBS
        // init makes ours the top-level filter and OBS's the "previous" one; on a fault
        // we record the exception details, then chain to OBS's filter so its formatted
        // report still reaches the handler installed above.
        unsafe {
            use windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
            let prev = SetUnhandledExceptionFilter(Some(native_exception_filter));
            let prev_ptr = prev.map(|f| f as usize).unwrap_or(0);
            PREV_EXCEPTION_FILTER.store(prev_ptr, Ordering::SeqCst);
        }
        info!("Installed fallback unhandled-exception filter");
    });
}

/// Top-level unhandled-exception filter (part 2). Records the fault to `crash.log`,
/// then chains to the previously-installed filter (OBS's) so its formatted report is
/// preserved.
unsafe extern "system" fn native_exception_filter(
    info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    write_exception_report(info);

    let prev = PREV_EXCEPTION_FILTER.load(Ordering::SeqCst);
    if prev != 0 {
        let prev_fn: unsafe extern "system" fn(
            *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
        ) -> i32 = std::mem::transmute(prev);
        return prev_fn(info);
    }
    // EXCEPTION_CONTINUE_SEARCH (0): no previous filter, let default handling proceed.
    0
}

/// Best-effort: write the exception code, faulting address and module into `crash.log`.
unsafe fn write_exception_report(
    info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) {
    if info.is_null() {
        return;
    }
    let record = (*info).ExceptionRecord;
    if record.is_null() {
        return;
    }
    let code = (*record).ExceptionCode.0 as u32;
    let address = (*record).ExceptionAddress as usize;
    let module = module_for_address(address).unwrap_or_else(|| "<unknown>".to_string());

    let sep = "=".repeat(80);
    let report = format!(
        "\n{sep}\nNATIVE EXCEPTION at {ts}\n{sep}\n\
         Exception code:  0x{code:08X}\n\
         Fault address:   0x{address:016X}\n\
         Faulting module: {module}\n{sep}\n",
        ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
    );
    crate::crash::append_to_crash_log(&report);
}

/// Resolve the module (DLL/exe) that owns `address`, returning its full path.
unsafe fn module_for_address(address: usize) -> Option<String> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };

    let mut module = HMODULE::default();
    // FROM_ADDRESS treats the pointer as an address to look up; UNCHANGED_REFCOUNT so we
    // do not have to (and, mid-crash, cannot safely) release the reference.
    GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        PCWSTR(address as *const u16),
        &mut module,
    )
    .ok()?;

    let mut buf = [0u16; 260];
    let len = GetModuleFileNameW(module, &mut buf);
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Spawn a background thread that owns a hidden top-level window so we receive
/// `WM_QUERYENDSESSION`/`WM_ENDSESSION` (part 4). On session end we log a
/// logoff/shutdown breadcrumb and clear the run marker, so an OS-driven session end
/// stops looking like a native crash.
pub fn watch_session_end() {
    if std::thread::Builder::new()
        .name("session-end-watch".into())
        .spawn(|| unsafe { run_session_end_window() })
        .is_err()
    {
        warn!("Could not spawn the session-end watcher thread");
    }
}

unsafe fn run_session_end_window() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HINSTANCE, HWND};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW, TranslateMessage,
        CW_USEDEFAULT, HMENU, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
    };

    let hmodule = match GetModuleHandleW(PCWSTR::null()) {
        Ok(h) => h,
        Err(e) => {
            warn!("session-end watcher: GetModuleHandleW failed: {e}");
            return;
        }
    };
    let hinstance = HINSTANCE(hmodule.0);

    // NUL-terminated wide strings that must outlive the RegisterClassW/CreateWindowExW
    // calls below (both borrow their pointers).
    let class_name: Vec<u16> = "CrowdCastSessionEndWatcher\0".encode_utf16().collect();
    let window_name: Vec<u16> = "crowd-cast session watcher\0".encode_utf16().collect();
    let class_ptr = PCWSTR(class_name.as_ptr());

    let wc = WNDCLASSW {
        lpfnWndProc: Some(session_wndproc),
        hInstance: hinstance,
        lpszClassName: class_ptr,
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 {
        warn!("session-end watcher: RegisterClassW failed");
        return;
    }

    // A real top-level window (parent = null), never shown (no WS_VISIBLE): the session
    // end messages are broadcast to top-level windows regardless of visibility, but NOT
    // to message-only (HWND_MESSAGE) windows.
    let created = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class_ptr,
        PCWSTR(window_name.as_ptr()),
        WINDOW_STYLE(0),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    );
    if let Err(e) = created {
        warn!("session-end watcher: CreateWindowExW failed: {e}");
        return;
    }

    info!("Session-end watcher window created");

    let mut msg = MSG::default();
    loop {
        let ret = GetMessageW(&mut msg, HWND::default(), 0, 0);
        // 0 = WM_QUIT, -1 = error; either way, stop pumping.
        if ret.0 <= 0 {
            break;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe extern "system" fn session_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_ENDSESSION, WM_QUERYENDSESSION,
    };

    match msg {
        // Grant the session end (do not block logoff/shutdown).
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            // wParam != 0 means the session is actually ending.
            if wparam.0 != 0 {
                let sep = "=".repeat(80);
                let report = format!(
                    "\n{sep}\nSESSION ENDING (logoff/shutdown) at {ts}\n{sep}\n",
                    ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                );
                crate::crash::append_to_crash_log(&report);
                error!("Session ending (logoff/shutdown)");
                // A known, logged termination cause; clear the marker so the next
                // launch does not misread this as a native crash.
                crate::crash::clear_run_marker();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
