//! crowd-cast Agent
//!
//! Captures paired screencast and input data for dataset collection.
//! Uses embedded libobs for single-binary distribution.

// Release builds run as a background tray agent, so use the Windows GUI
// subsystem to avoid popping a console window (which would show libobs/log
// output) when launched from the installer/Start Menu. Debug builds keep the
// console for development. No-op on non-Windows targets.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod capture;
mod config;
mod crash;
mod data;
mod input;
mod installer;
mod logging;
#[cfg(target_os = "linux")]
mod resume_linux;
mod sync;
mod ui;
mod upload;

use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Flag set by intentional exit paths (Quit menu, Sparkle update, Ctrl+C).
/// When main() exits, it checks this flag to decide the exit code:
///   true  → exit(0) → KeepAlive/Crashed does NOT restart (intentional)
///   false → exit(1) → KeepAlive/Crashed DOES restart (unexpected termination)
static INTENTIONAL_EXIT: AtomicBool = AtomicBool::new(false);

/// C-accessible function for the ObjC applicationShouldTerminate: delegate.
/// Returns true if the exit was intentional (Quit, Sparkle update, Ctrl+C).
#[no_mangle]
pub extern "C" fn is_intentional_exit() -> bool {
    INTENTIONAL_EXIT.load(Ordering::SeqCst)
}

/// Channel for the SIGINT handler to send shutdown commands.
#[cfg(unix)]
static CMD_SENDER_FOR_SIGNAL: std::sync::Mutex<
    Option<(mpsc::Sender<EngineCommand>, Arc<tokio::runtime::Runtime>)>,
> = std::sync::Mutex::new(None);

/// SIGINT handler: mark exit as intentional and trigger shutdown.
#[cfg(unix)]
extern "C" fn sigint_handler(_sig: libc::c_int) {
    INTENTIONAL_EXIT.store(true, Ordering::SeqCst);
    if let Ok(guard) = CMD_SENDER_FOR_SIGNAL.lock() {
        if let Some((ref tx, ref rt)) = *guard {
            let tx = tx.clone();
            rt.spawn(async move {
                let _ = tx.send(EngineCommand::Shutdown).await;
            });
        }
    }
    // Break the tray loop so main() can run its clean shutdown. macOS exits the Cocoa
    // run loop via the tray C library; Linux flips an atomic the ksni poll loop reads.
    #[cfg(all(not(no_tray), target_os = "macos"))]
    unsafe {
        ui::tray_ffi::tray_exit();
    }
    #[cfg(all(not(no_tray), target_os = "linux"))]
    ui::request_tray_exit();
}

/// Channel for the Windows console control handler to send shutdown commands.
#[cfg(windows)]
static WIN_CMD_SENDER: std::sync::Mutex<Option<mpsc::Sender<EngineCommand>>> =
    std::sync::Mutex::new(None);

/// Windows console control handler (Ctrl+C / Ctrl+Break / console close).
///
/// Windows has no SIGINT, so without this Ctrl+C hard-kills the process and the
/// current segment's buffered input events are never flushed to disk. This marks
/// the exit intentional and asks the engine to shut down gracefully (which runs
/// stop_recording → writes the segment's .msgpack). Returning TRUE keeps the
/// process alive long enough for main() to join the engine thread and flush.
#[cfg(windows)]
unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows::Win32::Foundation::BOOL {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            INTENTIONAL_EXIT.store(true, Ordering::SeqCst);
            if let Ok(guard) = WIN_CMD_SENDER.lock() {
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.try_send(EngineCommand::Shutdown);
                }
            }
            BOOL(1)
        }
        _ => BOOL(0),
    }
}

/// Windows power-event callback: on resume from suspend, ask the engine to restart the recording
/// fresh so keylog and video re-zero together (a recording that straddled a suspend has corrupt
/// timestamps). Registered via `PowerRegisterSuspendResumeNotification` with `DEVICE_NOTIFY_CALLBACK`.
/// The engine's wall-clock-gap check is the fallback if registration ever fails. Must return
/// ERROR_SUCCESS (0).
#[cfg(windows)]
unsafe extern "system" fn power_resume_callback(
    _context: *const core::ffi::c_void,
    event_type: u32,
    _setting: *const core::ffi::c_void,
) -> u32 {
    // PBT_APMRESUMESUSPEND (0x0007): resume after a user-initiated suspend.
    // PBT_APMRESUMEAUTOMATIC (0x0012): system woke itself (always delivered on resume).
    const PBT_APMRESUMESUSPEND: u32 = 0x0007;
    const PBT_APMRESUMEAUTOMATIC: u32 = 0x0012;
    if event_type == PBT_APMRESUMESUSPEND || event_type == PBT_APMRESUMEAUTOMATIC {
        if let Ok(guard) = WIN_CMD_SENDER.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.try_send(EngineCommand::ResumeFromSuspend);
            }
        }
    }
    0 // ERROR_SUCCESS
}

use config::Config;
use installer::{needs_setup, reconcile_autostart, run_wizard_gui, AutostartConfig};
use sync::{create_engine_channels, EngineCommand, SyncEngine};

/// One-shot marker the post-wizard re-exec sets on its replacement process (see the
/// wizard-completion branch in `main`). The replacement run reads and immediately
/// removes it, so children and later relaunches never inherit it — this is what makes
/// the post-setup sign-in prompt fire exactly once.
const POST_SETUP_ENV: &str = "CROWD_CAST_POST_SETUP";

/// Main entry point, runs tray on main thread (required for macOS)
fn main() -> Result<()> {
    // On Windows, declare Per-Monitor-V2 DPI awareness before any graphics or
    // window initialization. libobs (and its window/monitor capture sources) read
    // window rectangles in physical pixels; if the host process is DPI-unaware the
    // system virtualizes those coordinates on scaled displays (e.g. 150%), so the
    // captured window texture and the rect libobs uses disagree and the capture is
    // mis-scaled/cropped. OBS itself runs Per-Monitor-V2 for this reason.
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        // Best-effort: fails harmlessly if awareness was already set (e.g. via manifest).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // SPIKE-ONLY diagnostic: if this run is the WGC re-point measurement harness
    // (--measure-wgc-repoint <out_dir> ...), redirect logging (and crash.log, which
    // shares the same dir) into the caller's out_dir *before* logging::init_logging()
    // below fires -- resolve_log_dir() checks CROWD_CAST_LOG_PATH first. Otherwise this
    // diagnostic would append to the shared %LOCALAPPDATA%\crowd-cast\...\Logs tree the
    // long-running tray agent also writes to.
    #[cfg(target_os = "windows")]
    if std::env::var_os("CROWD_CAST_LOG_PATH").is_none() {
        let raw_args: Vec<String> = std::env::args().collect();
        if let Some(pos) = raw_args.iter().position(|a| a == "--measure-wgc-repoint") {
            if let Some(out_dir) = raw_args.get(pos + 1) {
                std::env::set_var(
                    "CROWD_CAST_LOG_PATH",
                    std::path::Path::new(out_dir).join("logs"),
                );
            }
        }
    }

    // Initialize logging
    let _log_guard = logging::init_logging()?;

    // Initialize crash handler (must be after logging so we have the log directory)
    let log_dir = logging::get_log_dir()?;
    match crash::init_crash_handler(&log_dir) {
        Ok(crash_log_path) => {
            info!("Crash handler initialized, crash log: {:?}", crash_log_path);
        }
        Err(e) => {
            warn!(
                "Failed to initialize crash handler: {} (crashes may not be logged)",
                e
            );
        }
    }

    info!("crowd-cast Agent starting...");

    // On Windows, register our notification identity (AUMID + Start Menu
    // shortcut) so toasts are branded as crowd-cast rather than PowerShell.
    #[cfg(target_os = "windows")]
    ui::register_notification_identity();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // Headless host-requirements diagnostic (Linux): print the same checks the
    // setup wizard gates on, then exit. Useful for support and CI.
    #[cfg(target_os = "linux")]
    {
        if args.iter().any(|a| a == "--check-requirements") {
            let autostart_desired = Config::load()
                .map(|c| c.capture.start_on_login)
                .unwrap_or(false);
            for r in installer::requirements::collect(autostart_desired) {
                let mark = if r.satisfied {
                    "OK"
                } else {
                    match r.severity {
                        installer::requirements::Severity::Required => "MISSING",
                        installer::requirements::Severity::Blocking => "MISSING",
                        installer::requirements::Severity::Recommended => "WARN",
                        installer::requirements::Severity::Optional => "OPTIONAL",
                    }
                };
                println!("[{:>8}] {}", mark, r.label);
                if !r.satisfied && !r.detail.is_empty() {
                    println!("           {}", r.detail);
                }
                if !r.satisfied && !r.command.is_empty() {
                    println!("           $ {}", r.command);
                }
            }
            return Ok(());
        }

        // Print the follow-focus provider's view of the focused app as you switch windows,
        // for ~15s, then exit. Diagnostic for the Linux follow-focus provider.
        if args.iter().any(|a| a == "--print-focus") {
            crate::capture::focus::ensure_started();
            println!("follow-focus diagnostic: switch focus between windows (~15s)...");
            let mut last = String::new();
            for _ in 0..150 {
                let live = crate::capture::focus::is_live();
                let focused = crate::capture::get_frontmost_app()
                    .map(|a| format!("{} (pid {})", a.bundle_id, a.pid))
                    .unwrap_or_else(|| "<none>".into());
                let cur = format!("live={live} focused={focused}");
                if cur != last {
                    println!("{cur}");
                    last = cur;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            return Ok(());
        }

        // X11 per-window capture diagnostic: report whether this session can do XComposite
        // per-app capture, and for each app identity passed after the flag, the window id the
        // source would bind. Follow-focus binds only the *focused* window, so an app resolves
        // only while it is focused — focus the app, then run e.g.
        //   crowd-cast-agent --print-windows firefox
        if let Some(pos) = args.iter().position(|a| a == "--print-windows") {
            let capable = crate::capture::x11_windows::x11_per_app_capable();
            println!("x11 per-app (XComposite) capable: {capable}");
            match crate::capture::get_main_display_resolution() {
                Ok((w, h)) => println!("display resolution: {w}x{h}"),
                Err(e) => println!("display resolution: <undetected: {e}>"),
            }
            let apps: Vec<&str> = args[pos + 1..]
                .iter()
                .map(String::as_str)
                .filter(|s| !s.starts_with('-'))
                .collect();
            if apps.is_empty() {
                println!("(pass app identities to resolve, e.g. --print-windows firefox code)");
            }
            for app in apps {
                match crate::capture::x11_windows::resolve_capture_window(app) {
                    Some(cw) => println!("  {app} -> {cw:?}"),
                    None => println!("  {app} -> <no resolvable window>"),
                }
            }
            return Ok(());
        }

        // List the app identities the wizard offers for capture, exactly as enumeration
        // produces them. On Wayland these are app_id/wm_class (from installed `.desktop`
        // entries); on X11 they are `/proc/comm`. Use this to confirm enumeration agrees with
        // what `--print-focus` reports for the same app.
        if args.iter().any(|a| a == "--list-apps") {
            let apps = crate::capture::list_capturable_apps();
            println!("capturable app identities ({}):", apps.len());
            for app in &apps {
                println!("  {}\t({})", app.bundle_id, app.name);
            }
            return Ok(());
        }

        // Internal: render the tray "Settings" app-selection panel in THIS process and write
        // the result as JSON to the given path, then exit. The agent process can't show GTK
        // itself: libobs's Wayland support runs a glib MainLoop on the default GMainContext
        // from a background thread, so once GTK is initialized in that process the two race
        // inside non-thread-safe GTK and crash (NULL GdkScreen -> SIGSEGV). The agent re-execs
        // us with this flag to get a clean, libobs-free process for the dialog; see
        // `ui::app_selector::show_panel`. This must stay ABOVE all libobs/tray/runtime init.
        if let Some(pos) = args.iter().position(|a| a == "--settings-panel-out") {
            let out = args
                .get(pos + 1)
                .ok_or_else(|| anyhow::anyhow!("--settings-panel-out requires a file path"))?;
            ui::app_selector::run_settings_panel_subprocess(std::path::Path::new(out))?;
            return Ok(());
        }

        // Internal: render the manual "Check for Updates" status dialog in a clean process.
        // The parent agent writes simple status snapshots to the supplied file while the
        // dialog polls it. This must stay above libobs/tray/runtime init for the same GTK
        // default-GMainContext reason as `--settings-panel-out`.
        if let Some(pos) = args.iter().position(|a| a == "--update-check-dialog") {
            let status = args
                .get(pos + 1)
                .ok_or_else(|| anyhow::anyhow!("--update-check-dialog requires a file path"))?;
            ui::update_dialog::run_update_check_dialog_subprocess(std::path::Path::new(status))?;
            return Ok(());
        }

        // Install + enable the bundled GNOME focus extension, then exit. This is the
        // command the wizard surfaces for the GNOME follow-focus prerequisite; it writes
        // no shell state beyond the per-user extensions dir and gsettings, and takes effect
        // after the next login (gnome-shell loads the extension at session start).
        if args.iter().any(|a| a == "--install-focus-extension") {
            match installer::gnome_focus::install_and_enable() {
                Ok(msg) => {
                    println!("{msg}");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("Failed to install focus extension: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    // SPIKE-ONLY Windows diagnostic: measure how long a window_capture (WGC) source
    // takes to resume producing frames of a new target window after being re-pointed at
    // runtime (obs_source_update / set_window_raw), and what the recording shows during
    // the gap (black vs. stale frames). Runs its own short-lived tokio runtime and writes
    // ONLY under the caller-supplied out_dir. Kept above config load / setup wizard /
    // autostart reconcile / auth / tray / engine init -- none of which this needs.
    if let Some(pos) = args.iter().position(|a| a == "--measure-wgc-repoint") {
        #[cfg(target_os = "windows")]
        {
            let usage = "usage: --measure-wgc-repoint <out_dir> <exeA> <exeB>";
            let out_dir = args.get(pos + 1).ok_or_else(|| anyhow::anyhow!(usage))?;
            let exe_a = args.get(pos + 2).ok_or_else(|| anyhow::anyhow!(usage))?;
            let exe_b = args.get(pos + 3).ok_or_else(|| anyhow::anyhow!(usage))?;
            match measure_wgc_repoint(std::path::Path::new(out_dir), exe_a, exe_b) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    eprintln!("measure-wgc-repoint failed: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = pos;
            eprintln!("--measure-wgc-repoint is windows only");
            std::process::exit(1);
        }
    }

    let force_setup = args.iter().any(|a| a == "--setup" || a == "-s");
    let missing_permissions = !installer::all_permissions_granted();

    // True only on the run re-exec'd by a just-completed setup wizard (the marker is
    // set in the wizard-completion branch below). Consumed immediately so OBS/dialog
    // child processes and later relaunches never inherit it.
    let post_setup_run = std::env::var(POST_SETUP_ENV).is_ok();
    std::env::remove_var(POST_SETUP_ENV);

    // Create tokio runtime for async operations
    let runtime = tokio::runtime::Runtime::new()?;

    // Initialize notifications early (best effort - non-fatal if it fails)
    let (notification_tx, notification_rx) = mpsc::unbounded_channel();
    if let Err(e) = ui::init_notifications(notification_tx) {
        warn!(
            "Failed to initialize notifications: {}. Display change alerts will not be shown.",
            e
        );
    }

    // Load configuration
    let mut config = Config::load()?;
    info!("Configuration loaded from {:?}", config.config_path());

    // On Linux, also re-show the wizard whenever a Required host component is missing
    // (e.g. the ScreenCast portal backend), or the saved config requires a capture mode
    // this host can't provide (e.g. per-app capture where it isn't available) -- so the
    // agent never runs with a config it cannot satisfy.
    #[cfg(target_os = "linux")]
    let requirements_unmet = installer::requirements::has_unmet_required();
    #[cfg(target_os = "linux")]
    let config_incompatible = !config.capture.target_apps.is_empty()
        && !installer::requirements::per_app_capture_available();
    #[cfg(not(target_os = "linux"))]
    let requirements_unmet = false;
    #[cfg(not(target_os = "linux"))]
    let config_incompatible = false;

    // start_on_login is a preference, but once set it becomes a precondition: on wlroots
    // compositors (sway/Hyprland/...) autostart needs a manual `exec` line the agent cannot
    // write itself. If the user opted into autostart but that line isn't present, the agent
    // must not silently run in a state that won't actually start on login -- gate on the
    // wizard until the line is pasted (detected on the post-wizard re-exec) or the user
    // disables start-on-login. No fallback, fail closed. (Other sessions either honor XDG
    // autostart or have no manual step, so `linux_manual_autostart()` is None there.)
    #[cfg(target_os = "linux")]
    let autostart_unsatisfied = config.capture.start_on_login
        && installer::autostart::linux_manual_autostart().is_some()
        && !installer::autostart::is_autostart_enabled();
    #[cfg(not(target_os = "linux"))]
    let autostart_unsatisfied = false;

    // Run setup wizard if needed
    if force_setup
        || needs_setup(&config)
        || missing_permissions
        || requirements_unmet
        || config_incompatible
        || autostart_unsatisfied
    {
        if autostart_unsatisfied {
            info!(
                "start_on_login is set but the autostart line is not in the compositor config \
                 -- opening setup until it is pasted or start-on-login is disabled"
            );
        }
        info!("Running setup wizard...");
        let result = run_wizard_gui(&mut config)?;

        if !result.completed {
            info!("Setup wizard was cancelled");
            std::process::exit(0);
        }

        info!("Setup completed successfully");

        // Re-exec the process to get a clean state for OBS initialization.
        // The wizard's GUI pollutes process state (graphics contexts, NSApplication, etc.)
        // in ways that cause libobs initialization to crash with SIGTRAP.
        info!("Restarting with clean process state...");

        let exe = std::env::current_exe()?;
        let filtered_args: Vec<String> = std::env::args()
            .skip(1) // skip the program name
            .filter(|a| a != "--setup" && a != "-s")
            .collect();

        // Use Unix exec to replace this process with a fresh one. The marker env var
        // tells the replacement run that setup JUST completed, so it shows the
        // one-time Google sign-in prompt (and consumes the marker right away).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&exe)
                .args(&filtered_args)
                .env(POST_SETUP_ENV, "1")
                .exec();
            // exec() only returns on error
            error!("exec failed: {}", err);
        }

        // Fallback for non-Unix or if exec fails
        std::process::Command::new(&exe)
            .args(&filtered_args)
            .env(POST_SETUP_ENV, "1")
            .spawn()?;
        std::process::exit(0);
    }

    reconcile_start_on_login(&mut config);

    // Check permissions
    let perms = installer::check_permissions();
    if !perms.accessibility.is_granted() {
        warn!("Accessibility permission not granted - input capture may not work");
    }
    if !perms.screen_recording.is_granted() {
        warn!("Screen Recording permission not granted - capture may not work");
    }

    // Bootstrap OBS binaries if needed
    info!("Bootstrapping OBS binaries...");
    let mut capture_ctx =
        match runtime.block_on(capture::CaptureContext::new(get_output_directory(&config))) {
            Ok(ctx) => ctx,
            Err(e) => {
                error!("Failed to bootstrap OBS binaries: {}", e);
                std::process::exit(1);
            }
        };
    info!("OBS binaries ready");

    // Heal pre-1096 LaunchAgent plists so launchd also relaunches after a clean
    // nonzero exit (KeepAlive.Crashed alone only covers signal deaths). Best-effort.
    if let Err(e) = installer::autostart::refresh_launch_agent_keepalive() {
        warn!("Could not refresh LaunchAgent KeepAlive: {}", e);
    }

    // Prime the capture mode + target list before initialize so the canvas can choose the
    // multi-monitor per-app envelope vs the display-capture canvas (setup_capture re-sets these).
    capture_ctx.set_single_active_app_capture(config.capture.single_active_app_capture);
    capture_ctx.set_mac_multi_monitor_capture(config.capture.mac_multi_monitor_capture);
    let target_apps = config.capture.target_apps.clone();
    capture_ctx.set_target_apps(&target_apps);

    // Initialize libobs + capture sources. On macOS this is retried with backoff:
    // the agent is commonly (re)launched right after a wake-time crash, while the
    // display list is still settling — the exact window in which SCK setup fails.
    // A clean exit(1) here would be FINAL (launchd's KeepAlive/Crashed only
    // relaunches signal deaths, not nonzero exits), so dying on the first attempt
    // turned a transient wake glitch into "agent gone until next login". Retrying
    // rides out the flux instead. Non-macOS keeps single-attempt semantics —
    // Linux capture setup can involve an interactive portal dialog, which must
    // not be re-prompted in a loop.
    #[cfg(target_os = "macos")]
    const STARTUP_RETRY_DELAYS_SECS: &[u64] = &[2, 5, 10, 20, 30];
    #[cfg(not(target_os = "macos"))]
    const STARTUP_RETRY_DELAYS_SECS: &[u64] = &[];

    let mut startup_attempt = 0usize;
    loop {
        let step_err = match capture_ctx.initialize() {
            Err(e) => Some(("initialize libobs", e)),
            Ok(()) => {
                match capture_ctx.setup_capture(&target_apps, &config.capture.restore_tokens) {
                    Err(e) => Some(("setup capture", e)),
                    Ok(_) => None,
                }
            }
        };
        match step_err {
            None => break,
            Some((step, e)) => {
                if startup_attempt < STARTUP_RETRY_DELAYS_SECS.len() {
                    let delay = STARTUP_RETRY_DELAYS_SECS[startup_attempt];
                    startup_attempt += 1;
                    warn!(
                        "Failed to {} ({}); displays may still be settling — retrying in {}s \
                         (attempt {}/{})",
                        step,
                        e,
                        delay,
                        startup_attempt,
                        STARTUP_RETRY_DELAYS_SECS.len()
                    );
                    std::thread::sleep(std::time::Duration::from_secs(delay));
                } else {
                    error!("Failed to {}: {}", step, e);
                    std::process::exit(1);
                }
            }
        }
    }
    info!("libobs context initialized");
    if target_apps.is_empty() {
        info!("Capture sources configured (display capture since no apps selected)");
    } else {
        info!(
            "Capture sources configured for {} applications: {:?}",
            target_apps.len(),
            target_apps
        );
    }

    // Linux/Wayland display capture: the first launch shows the portal monitor picker (a bare
    // slurp crosshair on wlroots/sway). Read back and persist its restore token under the
    // reserved display key so later launches restore the same output silently — no picker.
    // No-op once a token exists, and skipped on X11 (no portal, so the token never appears).
    #[cfg(target_os = "linux")]
    if target_apps.is_empty() && capture::is_wayland_session() {
        use std::time::{Duration, Instant};
        let key = capture::DISPLAY_CAPTURE_KEY;
        if !config.capture.restore_tokens.contains_key(key) {
            info!("Display capture: waiting for monitor selection to persist its restore token...");
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let tokens = capture_ctx.collect_restore_tokens();
                if let Some(token) = tokens.get(key) {
                    if !token.is_empty() {
                        config
                            .capture
                            .restore_tokens
                            .insert(key.to_string(), token.clone());
                        match config.save() {
                            Ok(()) => info!("Persisted display capture restore token"),
                            Err(e) => warn!("Failed to save display restore token: {}", e),
                        }
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    warn!("Timed out waiting for monitor selection; it will be requested again next launch.");
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }

    // Create engine channels
    let (cmd_tx, cmd_rx, status_tx, _status_rx) = create_engine_channels();

    // Initialize optional Google OAuth auth manager
    let auth_manager = option_env!("CROWD_CAST_GOOGLE_CLIENT_ID").map(|client_id| {
        let client_secret = option_env!("CROWD_CAST_GOOGLE_CLIENT_SECRET").unwrap_or("");
        let mgr = auth::AuthManager::new(client_id, client_secret);
        if mgr.is_authenticated() {
            info!("Authenticated as {}", mgr.email().unwrap_or("unknown"));
        }
        std::sync::Arc::new(tokio::sync::Mutex::new(mgr))
    });

    // Create sync engine
    let engine = SyncEngine::new(
        config.clone(),
        capture_ctx,
        cmd_rx,
        status_tx.clone(),
        notification_rx,
        auth_manager.clone(),
    )?;

    // Wrap runtime in Arc for sharing with signal handler
    let runtime = Arc::new(runtime);

    // Spawn the sync engine on the tokio runtime
    let engine_runtime = runtime.clone();
    let engine_handle = std::thread::spawn(move || {
        engine_runtime.block_on(async move {
            let mut engine = engine;
            if let Err(e) = engine.run().await {
                error!("Sync engine error: {}", e);
            }
        });
    });

    // One-time post-setup sign-in prompt, before the tray starts. The dialog blocks
    // only this (main) thread; the engine just spawned keeps recording behind it.
    if post_setup_run {
        prompt_post_setup_signin(&auth_manager, &runtime);
    }

    // Linux: listen for resume-from-suspend via logind and restart the recording fresh, so a
    // recording that straddled a sleep doesn't drift (keylog↔video re-zero). Primary signal;
    // the engine's wall-clock-gap check is the fallback. macOS uses its restart-on-unlock path;
    // Windows uses the power callback registered below.
    #[cfg(target_os = "linux")]
    {
        let resume_tx = cmd_tx.clone();
        runtime.spawn(resume_linux::run(resume_tx));
    }

    // Handle SIGINT (Ctrl+C) only. SIGTERM is intentionally NOT caught so that
    // macOS sleep/hibernate termination produces a non-zero exit, which triggers
    // KeepAlive/Crashed restart. Ctrl+C sets INTENTIONAL_EXIT so the process
    // exits with 0 (no restart).
    #[cfg(unix)]
    {
        let sigint_tx = cmd_tx.clone();
        let sigint_runtime = runtime.clone();
        unsafe {
            libc::signal(libc::SIGINT, sigint_handler as libc::sighandler_t);
        }
        // Store the channel for the signal handler to use
        *CMD_SENDER_FOR_SIGNAL.lock().unwrap() = Some((sigint_tx, sigint_runtime));
    }

    // Windows: install a console control handler so Ctrl+C (and console close)
    // shut the engine down gracefully and flush the current segment to disk,
    // instead of hard-killing the process and losing buffered input events.
    #[cfg(windows)]
    {
        *WIN_CMD_SENDER.lock().unwrap() = Some(cmd_tx.clone());
        unsafe {
            use windows::Win32::Foundation::BOOL;
            use windows::Win32::System::Console::SetConsoleCtrlHandler;
            if let Err(e) = SetConsoleCtrlHandler(Some(console_ctrl_handler), BOOL(1)) {
                warn!("Failed to install console Ctrl+C handler: {}", e);
            }
        }

        // Register for resume-from-suspend so a recording that slept gets restarted fresh
        // (keylog↔video re-zero); the engine's wall-clock-gap check is the fallback if this
        // registration fails. Callback mode needs no window. The subscribe-params struct is
        // leaked so it lives for the process lifetime (the OS reads it past this call), as is
        // the returned handle (we never unregister — the registration lasts the whole run).
        unsafe {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Power::{
                PowerRegisterSuspendResumeNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
            };
            use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

            // Leaked so the struct outlives this call — the OS retains the callback pointer.
            let params = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(power_resume_callback),
                Context: std::ptr::null_mut(),
            }));
            // Out-param (`*mut *mut c_void` per the API): the OS writes the registration handle
            // here. We never unregister (the registration lasts the whole run), so it's unused.
            let mut registration: *mut core::ffi::c_void = std::ptr::null_mut();
            let status = PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                HANDLE(params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut core::ffi::c_void),
                &mut registration as *mut *mut core::ffi::c_void,
            );
            if status.is_ok() {
                info!("Registered for resume-from-suspend notifications");
            } else {
                warn!(
                    "Failed to register suspend/resume notification ({:?}); relying on wall-clock-gap fallback",
                    status
                );
            }
            let _ = registration;
        }
    }

    // Run tray on main thread
    #[cfg(not(no_tray))]
    {
        let tray_cmd_tx = cmd_tx.clone();
        let tray_status_rx = status_tx.subscribe();

        info!("Starting system tray on main thread");

        match ui::TrayApp::new(
            tray_cmd_tx,
            tray_status_rx,
            auth_manager.clone(),
            Some(runtime.clone()),
        ) {
            Ok(tray) => {
                if let Err(e) = tray.run() {
                    error!("Tray error: {}", e);
                }
            }
            Err(e) => {
                error!("Failed to create tray: {}", e);
            }
        }

        info!("Tray exited, shutting down...");
    }

    #[cfg(no_tray)]
    {
        info!("System tray not available on this platform");
        info!("Press Ctrl+C to exit...");

        // Wait for engine to finish (Ctrl+C handler will send shutdown)
        engine_handle.join().ok();
    }

    // Send shutdown command to engine (in case tray exited without Ctrl+C), then wait for
    // the engine thread. On no_tray builds (Windows) the engine was already joined above
    // (after the Ctrl+C handler sent Shutdown), so this is skipped to avoid a double join.
    #[cfg(not(no_tray))]
    {
        runtime.block_on(async {
            let _ = cmd_tx.send(EngineCommand::Shutdown).await;
        });

        // Wait for engine thread to finish. The no_tray path above already joined
        // engine_handle while waiting for Ctrl+C, so this (tray-owned) path is the
        // only other join — no double join.
        let _ = engine_handle.join();
    }

    // Determine exit code: intentional exits (Quit menu, Sparkle update, Ctrl+C)
    // exit with 0 so KeepAlive/Crashed does NOT restart. All other exits
    // (SIGTERM from macOS sleep/hibernate, NSApp termination) exit with 1
    // so KeepAlive/Crashed DOES restart on next login.
    #[cfg(not(no_tray))]
    let intentional = INTENTIONAL_EXIT.load(Ordering::SeqCst) || ui::was_quit_requested();
    #[cfg(no_tray)]
    let intentional = INTENTIONAL_EXIT.load(Ordering::SeqCst);

    if intentional {
        info!("Intentional shutdown — exiting with code 0 (no restart)");
        std::process::exit(0);
    } else {
        info!("Unexpected shutdown — exiting with code 1 (KeepAlive will restart)");
        std::process::exit(1);
    }
}

fn get_output_directory(config: &Config) -> std::path::PathBuf {
    config
        .recording
        .output_directory
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-recordings"))
}

/// SPIKE-ONLY (WGC re-point measurement harness). Driven by the --measure-wgc-repoint
/// branch in `main`. Windows-only.
///
/// Points a `window_capture` (WGC) source at exeA's window, records, then re-points it at
/// runtime (obs_source_update / set_window_raw) between exeA and exeB and logs what OBS
/// reports around each re-point, so we can measure how long a re-point takes to resume
/// producing new-window frames and whether the recording shows black vs. stale frames
/// during the gap. Writes a recording mp4, a JSONL event log, and a final summary line --
/// all under `out_dir`. Never touches the prod agent's config/data tree.
#[cfg(target_os = "windows")]
fn measure_wgc_repoint(out_dir: &std::path::Path, exe_a: &str, exe_b: &str) -> Result<()> {
    use anyhow::Context as _;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("Failed to create output dir {:?}", out_dir))?;

    // 1. Resolve one window obs_id per exe (first match by exe file stem, case-insensitive)
    //    from a single enumeration -- done once, outside any timed window (enumeration is
    //    expensive; see recon). Abort with a clear message if either exe has no window.
    let windows =
        capture::enumerate_capturable_windows().context("Failed to enumerate windows")?;
    let resolve = |exe: &str| -> Option<(String, Option<String>)> {
        windows
            .iter()
            .find(|(_, w_exe, _)| w_exe.eq_ignore_ascii_case(exe))
            .map(|(obs_id, _, title)| (obs_id.clone(), title.clone()))
    };
    let (obs_id_a, title_a) = resolve(exe_a)
        .ok_or_else(|| anyhow::anyhow!("no capturable (non-minimized) window for exeA '{}'", exe_a))?;
    let (obs_id_b, title_b) = resolve(exe_b)
        .ok_or_else(|| anyhow::anyhow!("no capturable (non-minimized) window for exeB '{}'", exe_b))?;
    info!(
        "measure-wgc-repoint: A '{}' -> obs_id '{}' (title {:?}); B '{}' -> obs_id '{}' (title {:?})",
        exe_a, obs_id_a, title_a, exe_b, obs_id_b, title_b
    );

    // 2. Build the capture context targeting exeA in single-active mode. Own short-lived
    //    runtime so we never touch the main path's config load / wizard / tray / engine.
    let runtime = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let mut ctx = runtime
        .block_on(capture::CaptureContext::new(out_dir.to_path_buf()))
        .context("Failed to create capture context")?;
    let targets = vec![exe_a.to_string()];
    ctx.set_single_active_app_capture(true);
    ctx.set_target_apps(&targets);
    ctx.initialize().context("Failed to initialize libobs")?;
    ctx.setup_capture(&targets, &std::collections::HashMap::new())
        .context("Failed to set up capture")?;

    // Force exeA's scene onto channel 0. setup_capture only auto-activates it if exeA was
    // frontmost; we cannot (and must not) change focus, so activate it explicitly. app_scenes
    // is keyed by the canonical (lower-cased) exe stem.
    ctx.switch_active_app_capture(Some(&exe_a.to_ascii_lowercase()))
        .context("Failed to activate exeA capture scene")?;

    // Wait until the source actually produces non-zero frames before recording (WGC starts
    // asynchronously); bail if it never does (e.g. exeA's window vanished after step 1).
    let ready_deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !ctx.active_source_is_ready().unwrap_or(false) {
        if Instant::now() >= ready_deadline {
            anyhow::bail!("exeA capture source never became ready (no frames after 5s)");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // 2b. Record each window's initial capture dimensions and guard against the two
    //     windows sharing a source size. The in-log transition signal
    //     (measure_wgc_poll_dims) keys purely on obs_source_get_width/height changing, so
    //     if exeA and exeB capture at the SAME pixel size (e.g. both maximized on one
    //     monitor -- a common real setup) NO "dims" event ever fires across a re-point and
    //     the dims stream is blind. That is why the PRIMARY signal is per-frame mp4 content
    //     analysed against the recording_start anchor logged below; "dims" is only a coarse
    //     secondary cue. exeA is currently active and ready so read its dims directly; probe
    //     exeB by pointing the source at it and waiting for its dims to appear (bounded), then
    //     point back at exeA. All of this runs BEFORE start_recording, so it never appears in
    //     the measured mp4.
    let dims_a = ctx.active_source_dimensions().unwrap_or(None);
    ctx.spike_repoint_active_to_raw_window(&obs_id_b)
        .context("probe: failed to point capture source at exeB")?;
    let probe_deadline = Instant::now() + std::time::Duration::from_secs(3);
    let mut dims_b = ctx.active_source_dimensions().unwrap_or(None);
    while dims_b == dims_a && Instant::now() < probe_deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        dims_b = ctx.active_source_dimensions().unwrap_or(None);
    }
    ctx.spike_repoint_active_to_raw_window(&obs_id_a)
        .context("probe: failed to point capture source back at exeA")?;
    let reready_deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !ctx.active_source_is_ready().unwrap_or(false) {
        if Instant::now() >= reready_deadline {
            anyhow::bail!("exeA capture source never became ready again after exeB dims probe");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    info!(
        "measure-wgc-repoint: initial capture dims A={:?} B={:?}",
        dims_a, dims_b
    );
    if dims_a.is_some() && dims_a == dims_b {
        warn!(
            "measure-wgc-repoint: exeA and exeB capture at identical dimensions {:?}; the \
             \"dims\" event stream will show NO transition across re-points. Rely on per-frame \
             mp4 content (the primary signal) located via the recording_start anchor; treat \
             \"dims\" as only a coarse secondary cue.",
            dims_a
        );
    }

    // 3. Start recording; remember the mp4 path.
    let session_id = format!(
        "wgc-measure-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let session = ctx
        .start_recording(session_id)
        .context("Failed to start recording")?;
    let mp4_path = session.output_path.clone();
    // The OBS video-frame-time captured at record start IS the mp4's PTS t=0 (mirrors
    // engine.rs: video_relative_us = (get_video_frame_time() - start_time_ns)/1000). Every
    // event below logs frame_time_ns on this same monotonic clock, so logging this anchor
    // once is what lets an analyst convert any event to video-relative time and locate the
    // switch/switch_done events in the recording. wall_ms is a different, unanchored clock.
    let start_time_ns = session.start_time_ns;

    // 4. Open the JSONL event log.
    let log_path = out_dir.join("measure_log.jsonl");
    let mut log = std::fs::File::create(&log_path)
        .with_context(|| format!("Failed to create log file {:?}", log_path))?;
    let start = Instant::now();

    // First line: the mp4 t=0 anchor, so every later frame_time_ns maps onto the
    // recording's zero-based PTS timeline.
    measure_wgc_emit(
        &mut log,
        &ctx,
        start,
        serde_json::json!({"event": "recording_start", "start_time_ns": start_time_ns}),
    )?;

    // Run the timed body; on ANY failure still attempt stop_recording before returning,
    // so the mp4 is finalized and playable regardless.
    let body = measure_wgc_repoint_body(
        &mut ctx,
        &mut log,
        start,
        (exe_a, obs_id_a.as_str()),
        (exe_b, obs_id_b.as_str()),
    );
    let _ = ctx.stop_recording();
    body?;

    // 8. Final summary line on stdout.
    let (cw, ch) = ctx.canvas_dimensions();
    let summary = serde_json::json!({
        "mp4": mp4_path.to_string_lossy(),
        "log": log_path.to_string_lossy(),
        // mp4 PTS t=0 anchor (also emitted as the recording_start log line): map any
        // event's frame_time_ns onto the video via (frame_time_ns - start_time_ns)/1000 us.
        "start_time_ns": start_time_ns,
        "dims_a": dims_a.map(|(w, h)| [w, h]),
        "dims_b": dims_b.map(|(w, h)| [w, h]),
        "dims_differ": dims_a != dims_b,
        "primary_signal": "per-frame mp4 content vs recording_start anchor; dims is a coarse secondary cue",
        "switches": 20,
        "noops": 10,
        "canvas": [cw, ch],
    });
    println!("{summary}");
    Ok(())
}

/// SPIKE-ONLY: the timed measurement body (warmup, alternating re-points, same-window
/// no-ops). Split out from `measure_wgc_repoint` so its caller can always run
/// stop_recording afterwards even if a step here fails. `a`/`b` are `(exe, obs_id)`.
#[cfg(target_os = "windows")]
fn measure_wgc_repoint_body(
    ctx: &mut capture::CaptureContext,
    log: &mut std::fs::File,
    start: std::time::Instant,
    a: (&str, &str),
    b: (&str, &str),
) -> Result<()> {
    use anyhow::Context as _;
    use std::time::Duration;

    let (exe_a, obs_a) = a;
    let (exe_b, obs_b) = b;

    // Carries the last-seen active-source dims across every phase so a dims change is
    // detected relative to the true prior value, not per-phase.
    let mut last_dims: Option<(u32, u32)> = None;

    // 5. Warmup: 5s, logging a "dims" line on every change (the first observation included).
    measure_wgc_poll_dims(log, ctx, start, Duration::from_secs(5), &mut last_dims)?;

    // 6. Alternating re-points: 20 total, starting A->B (even n -> B, odd n -> back to A),
    //    3000ms dwell between them polling dims every 20ms.
    let mut current_obs = obs_a.to_string(); // source starts showing exeA's window
    for n in 0..20u32 {
        let (to_exe, to_obs) = if n % 2 == 0 { (exe_b, obs_b) } else { (exe_a, obs_a) };
        measure_wgc_emit(
            log,
            ctx,
            start,
            serde_json::json!({"event": "switch", "n": n, "to": to_exe, "obs_id": to_obs}),
        )?;
        ctx.spike_repoint_active_to_raw_window(to_obs)
            .with_context(|| format!("re-point #{} to '{}' failed", n, to_exe))?;
        current_obs = to_obs.to_string();
        measure_wgc_emit(log, ctx, start, serde_json::json!({"event": "switch_done", "n": n}))?;
        measure_wgc_poll_dims(log, ctx, start, Duration::from_millis(3000), &mut last_dims)?;
    }

    // 7. Same-window no-ops: 10 re-points to the CURRENTLY SET obs_id, 2000ms dwell.
    for n in 0..10u32 {
        measure_wgc_emit(
            log,
            ctx,
            start,
            serde_json::json!({"event": "noop", "n": n, "obs_id": current_obs.as_str()}),
        )?;
        ctx.spike_repoint_active_to_raw_window(&current_obs)
            .with_context(|| format!("no-op re-point #{} failed", n))?;
        measure_wgc_emit(log, ctx, start, serde_json::json!({"event": "noop_done", "n": n}))?;
        measure_wgc_poll_dims(log, ctx, start, Duration::from_millis(2000), &mut last_dims)?;
    }

    Ok(())
}

/// SPIKE-ONLY: write one JSONL event line, augmented with wall-clock ms since harness
/// start and the OBS video frame time (ns) when available (null otherwise).
#[cfg(target_os = "windows")]
fn measure_wgc_emit(
    log: &mut std::fs::File,
    ctx: &capture::CaptureContext,
    start: std::time::Instant,
    mut event: serde_json::Value,
) -> Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;

    if let Some(obj) = event.as_object_mut() {
        obj.insert(
            "wall_ms".to_string(),
            serde_json::json!(start.elapsed().as_millis() as u64),
        );
        let frame_time = match ctx.get_video_frame_time() {
            Ok(ns) => serde_json::json!(ns),
            Err(_) => serde_json::Value::Null,
        };
        obj.insert("frame_time_ns".to_string(), frame_time);
    }
    writeln!(log, "{event}").context("Failed to write measure log line")?;
    Ok(())
}

/// SPIKE-ONLY: poll `active_source_dimensions()` every 20ms for `dwell`, writing a
/// `{"event":"dims",...}` line on every change (relative to `last`, which is updated).
#[cfg(target_os = "windows")]
fn measure_wgc_poll_dims(
    log: &mut std::fs::File,
    ctx: &capture::CaptureContext,
    start: std::time::Instant,
    dwell: std::time::Duration,
    last: &mut Option<(u32, u32)>,
) -> Result<()> {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + dwell;
    loop {
        let dims = ctx.active_source_dimensions().unwrap_or(None);
        if dims != *last {
            *last = dims;
            let (w, h) = match dims {
                Some((w, h)) => (serde_json::json!(w), serde_json::json!(h)),
                None => (serde_json::Value::Null, serde_json::Value::Null),
            };
            measure_wgc_emit(
                log,
                ctx,
                start,
                serde_json::json!({"event": "dims", "w": w, "h": h}),
            )?;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn reconcile_start_on_login(config: &mut Config) {
    if !config.capture.setup_completed {
        return;
    }

    let desired = config.capture.start_on_login;

    let autostart_config = AutostartConfig::default();
    match reconcile_autostart(&autostart_config, desired) {
        Ok(_) => info!("Autostart reconciled (start_on_login={})", desired),
        Err(e) => warn!("Failed to reconcile autostart: {}", e),
    }
}

/// One-time Google sign-in prompt shown on the run right after a completed setup
/// wizard (see `POST_SETUP_ENV`).
///
/// The wizard itself never mentions login, so without this the only sign-in
/// affordance is the tray menu item and participants record indefinitely under an
/// anonymous ID. Always skippable — a hard gate would strand users whose OAuth flow
/// fails and dead-end builds compiled without a Google client ID (where
/// `auth_manager` is `None` and no prompt appears at all) — and it never repeats:
/// the tray item remains the ongoing affordance.
fn prompt_post_setup_signin(
    auth_manager: &Option<Arc<tokio::sync::Mutex<auth::AuthManager>>>,
    runtime: &Arc<tokio::runtime::Runtime>,
) {
    let Some(auth) = auth_manager else {
        info!("Post-setup sign-in prompt skipped: Google OAuth not configured in this build");
        return;
    };

    let already_signed_in = runtime.block_on(async { auth.lock().await.is_authenticated() });
    if already_signed_in {
        return;
    }

    if !show_post_setup_signin_dialog() {
        // On Linux no dialog is shown and the platform fn logs its own pointer.
        #[cfg(not(target_os = "linux"))]
        info!("User chose to continue anonymously; sign-in available anytime from the tray menu");
        return;
    }

    info!("Post-setup prompt accepted: starting Google sign-in flow...");
    // Same pattern as the tray's sign-in handler (ui/tray.rs handle_sign_in): run the
    // OAuth flow (browser + blocking localhost callback) on a background thread so an
    // abandoned login can never stall engine or tray startup.
    let auth = auth.clone();
    let rt = runtime.clone();
    std::thread::spawn(move || {
        let result = rt.block_on(async {
            let mut mgr = auth.lock().await;
            mgr.login().await
        });
        match result {
            Ok(state) => {
                info!("Post-setup sign-in successful: {}", state.email);
                // Tell the tray loop to refresh its account display (the tray may
                // already be running by the time the user finishes in the browser).
                ui::notify_sign_in_completed();
            }
            Err(e) => {
                error!(
                    "Post-setup sign-in failed: {} (you can sign in later from the tray menu)",
                    e
                );
            }
        }
    });
}

/// Show the post-setup sign-in dialog. Returns true if the user chose to sign in.
/// Blocking modal NSAlert (see wizard_darwin.m); must run on the main thread, which
/// is where `main` calls it (pre-tray).
#[cfg(target_os = "macos")]
fn show_post_setup_signin_dialog() -> bool {
    extern "C" {
        fn show_post_setup_signin_prompt() -> std::os::raw::c_int;
    }
    unsafe { show_post_setup_signin_prompt() == 1 }
}

/// Show the post-setup sign-in dialog. Returns true if the user chose to sign in.
/// Plain MessageBoxW via nwg — no `nwg::init()` required. Native message boxes have
/// fixed Yes/No button labels, so the sign-in-vs-anonymous meaning is carried by the
/// message text (macOS gets real "Sign In" / "Continue Anonymously" buttons).
#[cfg(target_os = "windows")]
fn show_post_setup_signin_dialog() -> bool {
    use native_windows_gui as nwg;
    let choice = nwg::message(&nwg::MessageParams {
        title: "Sign in to CrowdCast",
        content: "Sign in with your Google account so your contributions are credited to you?\n\n\
                  Yes: sign in now.\n\
                  No: continue anonymously (recordings stay linked to a random ID only; \
                  you can sign in any time from the tray icon).",
        buttons: nwg::MessageButtons::YesNo,
        icons: nwg::MessageIcons::Question,
    });
    choice == nwg::MessageChoice::Yes
}

/// Linux: no native dialog. GTK must never be initialized inside this libobs-owning
/// process (libobs's Wayland path runs a GLib loop on the default main context — see
/// the `--settings-panel-out` comment above), so a dialog would need the
/// clean-subprocess dance, which is disproportionate for a one-time prompt. Log the
/// pointer instead; the tray's "Sign in with Google" item is the affordance.
#[cfg(target_os = "linux")]
fn show_post_setup_signin_dialog() -> bool {
    info!(
        "Setup complete: recording anonymously. Sign in with Google from the tray menu \
         to have your contributions credited to you"
    );
    false
}

fn print_help() {
    println!("crowd-cast Agent - Paired screencast and input capture");
    println!();
    println!("USAGE:");
    println!("    crowd-cast-agent [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help    Print this help message");
    println!("    -s, --setup   Run the setup wizard");
    #[cfg(target_os = "linux")]
    {
        println!("        --check-requirements");
        println!("                  Print host requirement checks and exit (Linux)");
        println!("        --install-focus-extension");
        println!("                  Install + enable the GNOME follow-focus extension (Linux)");
        println!("        --print-focus");
        println!("                  Diagnostic: print the focused app for ~15s (Linux)");
        println!("        --print-windows [APP...]");
        println!("                  Diagnostic: report X11 per-app capability and resolve each");
        println!("                  APP identity to its XComposite capture window (Linux/X11)");
        println!("        --list-apps");
        println!("                  Diagnostic: print the app identities offered for capture");
    }
    println!();
    println!("ENVIRONMENT:");
    println!("    RUST_LOG      Set log level (e.g., debug, info, warn)");
    println!("    CROWD_CAST_LOG_PATH");
    println!(
        "                  Override log directory (default: ~/Library/Logs/crowd-cast on macOS)"
    );
    println!();
    println!("For more information, visit: https://github.com/p-doom/crowd-cast");
}
