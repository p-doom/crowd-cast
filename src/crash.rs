//! Crash handling and diagnostics
//!
//! This module sets up handlers to capture crash information for all failure modes:
//! - Rust panics (with full backtraces)
//! - Unix signals (SIGSEGV, SIGABRT, SIGBUS, etc.)
//!
//! Crash information is written to a dedicated crash log file that's flushed
//! synchronously to survive process termination.

use std::fs::OpenOptions;
use std::io::Write;
use std::panic::PanicInfo;
use std::path::PathBuf;
use std::sync::OnceLock;
use tracing::error;

/// Global crash log file path, set during initialization
static CRASH_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Global crash log file descriptor for signal handlers (must be set before signals)
#[cfg(unix)]
static CRASH_LOG_FD: OnceLock<std::os::unix::io::RawFd> = OnceLock::new();

const CRASH_LOG_FILENAME: &str = "crash.log";

/// Initialize crash handling. Call this early in main().
///
/// Sets up:
/// - Panic hook with backtrace logging
/// - Signal handlers for SIGSEGV, SIGABRT, SIGBUS, SIGFPE, SIGILL
///
/// Returns the path to the crash log file.
pub fn init_crash_handler(log_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let crash_log_path = log_dir.join(CRASH_LOG_FILENAME);
    
    // Store path globally for panic hook
    let _ = CRASH_LOG_PATH.set(crash_log_path.clone());
    
    // Open/create crash log file and store fd for signal handler
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_log_path)?;
        
        let fd = file.as_raw_fd();
        // Duplicate fd so it stays open after File is dropped
        let dup_fd = unsafe { libc::dup(fd) };
        if dup_fd >= 0 {
            let _ = CRASH_LOG_FD.set(dup_fd);
        }
    }
    
    // Set up panic hook
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        handle_panic(panic_info);
        default_hook(panic_info);
    }));
    
    // Set up signal handlers
    #[cfg(unix)]
    unsafe {
        install_signal_handlers();
    }
    
    Ok(crash_log_path)
}

/// Handle a Rust panic by logging it to the crash log
fn handle_panic(panic_info: &PanicInfo) {
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    
    // Get panic message
    let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "Unknown panic payload".to_string()
    };
    
    // Get location
    let location = if let Some(loc) = panic_info.location() {
        format!("{}:{}:{}", loc.file(), loc.line(), loc.column())
    } else {
        "unknown location".to_string()
    };
    
    // Get backtrace
    let backtrace = std::backtrace::Backtrace::force_capture();
    
    // Format crash report
    let separator = "=".repeat(80);
    let report = format!(
        "\n{sep}\n\
         PANIC at {ts}\n\
         {sep}\n\
         Location: {loc}\n\
         Message: {msg}\n\
         \n\
         Backtrace:\n\
         {bt}\n\
         {sep}\n",
        sep = separator,
        ts = timestamp,
        loc = location,
        msg = message,
        bt = backtrace
    );
    
    // Write to crash log (synchronously)
    if let Some(path) = CRASH_LOG_PATH.get() {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(report.as_bytes());
            let _ = file.flush();
            let _ = file.sync_all();
        }
    }
    
    // Also log via tracing (may not be flushed if we're crashing)
    error!(
        "PANIC at {}: {} (see crash.log for full backtrace)",
        location, message
    );
}

#[cfg(unix)]
unsafe fn install_signal_handlers() {
    use libc::{
        sigaction, sighandler_t, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGTRAP,
        SA_RESETHAND, SA_SIGINFO,
    };
    
    // Signals to catch
    let signals = [
        (SIGSEGV, "SIGSEGV (Segmentation fault)"),
        (SIGABRT, "SIGABRT (Abort)"),
        (SIGBUS, "SIGBUS (Bus error)"),
        (SIGFPE, "SIGFPE (Floating point exception)"),
        (SIGILL, "SIGILL (Illegal instruction)"),
        (SIGTRAP, "SIGTRAP (Trace/breakpoint trap)"),
    ];
    
    for (sig, _name) in signals {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = signal_handler as sighandler_t;
        action.sa_flags = SA_RESETHAND | SA_SIGINFO; // Reset to default after handling
        libc::sigemptyset(&mut action.sa_mask);
        
        sigaction(sig, &action, std::ptr::null_mut());
    }
}

/// Signal handler - must only use async-signal-safe functions!
#[cfg(unix)]
extern "C" fn signal_handler(sig: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    // SAFETY: We only use async-signal-safe functions here:
    // - write() to a file descriptor
    // - _exit()
    
    let signal_name = match sig {
        libc::SIGSEGV => "SIGSEGV (Segmentation fault)",
        libc::SIGABRT => "SIGABRT (Abort)",
        libc::SIGBUS => "SIGBUS (Bus error)",
        libc::SIGFPE => "SIGFPE (Floating point exception)",
        libc::SIGILL => "SIGILL (Illegal instruction)",
        libc::SIGTRAP => "SIGTRAP (Trace/breakpoint trap)",
        _ => "Unknown signal",
    };
    
    // Get fault address if available
    let fault_addr = if !info.is_null() {
        unsafe { (*info).si_addr() as usize }
    } else {
        0
    };
    
    // Build message using only stack-allocated buffer (no heap allocation!)
    let mut buf = [0u8; 512];
    let msg = format_signal_message(&mut buf, sig, signal_name, fault_addr);
    
    // Write to crash log fd (async-signal-safe)
    if let Some(&fd) = CRASH_LOG_FD.get() {
        unsafe {
            libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::fsync(fd);
        }
    }
    
    // Also write to stderr
    unsafe {
        libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
    }
    
    // Re-raise the signal with default handler to generate core dump / proper exit
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Format signal message without heap allocation (async-signal-safe)
#[cfg(unix)]
fn format_signal_message<'a>(buf: &'a mut [u8; 512], sig: i32, name: &str, addr: usize) -> &'a [u8] {
    // Manual formatting to avoid allocation
    let mut pos = 0;
    
    // Header
    let header = b"\n================================================================================\nCRASH: ";
    buf[pos..pos + header.len()].copy_from_slice(header);
    pos += header.len();
    
    // Signal name
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(buf.len() - pos - 100);
    buf[pos..pos + name_len].copy_from_slice(&name_bytes[..name_len]);
    pos += name_len;
    
    // Signal number
    let sig_prefix = b" (signal ";
    buf[pos..pos + sig_prefix.len()].copy_from_slice(sig_prefix);
    pos += sig_prefix.len();
    
    // Format signal number
    pos += format_int(&mut buf[pos..], sig as usize);
    
    buf[pos] = b')';
    pos += 1;
    
    // Fault address
    if addr != 0 {
        let addr_prefix = b"\nFault address: 0x";
        buf[pos..pos + addr_prefix.len()].copy_from_slice(addr_prefix);
        pos += addr_prefix.len();
        pos += format_hex(&mut buf[pos..], addr);
    }
    
    // Footer
    let footer = b"\n================================================================================\n";
    let footer_len = footer.len().min(buf.len() - pos);
    buf[pos..pos + footer_len].copy_from_slice(&footer[..footer_len]);
    pos += footer_len;
    
    &buf[..pos]
}

/// Format an integer without allocation
#[cfg(unix)]
fn format_int(buf: &mut [u8], mut n: usize) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    
    let mut tmp = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    
    // Reverse into output buffer
    for j in 0..i {
        buf[j] = tmp[i - 1 - j];
    }
    i
}

/// Format a hex number without allocation
#[cfg(unix)]
fn format_hex(buf: &mut [u8], mut n: usize) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    
    let hex_chars = b"0123456789abcdef";
    let mut tmp = [0u8; 16];
    let mut i = 0;
    while n > 0 {
        tmp[i] = hex_chars[n & 0xf];
        n >>= 4;
        i += 1;
    }
    
    // Reverse into output buffer
    for j in 0..i {
        buf[j] = tmp[i - 1 - j];
    }
    i
}

/// Log a critical operation marker to the crash log.
/// Call this before operations that might crash to help diagnose where crashes occur.
pub fn log_critical_operation(operation: &str) {
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    let msg = format!("[{}] CRITICAL_OP: {}\n", timestamp, operation);
    
    if let Some(path) = CRASH_LOG_PATH.get() {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(msg.as_bytes());
            let _ = file.flush();
        }
    }
}

/// Get the crash log path
pub fn get_crash_log_path() -> Option<&'static PathBuf> {
    CRASH_LOG_PATH.get()
}
