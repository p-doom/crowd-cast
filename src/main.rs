//! crowd-cast Agent
//!
//! Captures paired screencast and input data for dataset collection.
//! Uses embedded libobs for single-binary distribution.

mod auth;
mod capture;
mod config;
mod crash;
mod data;
mod input;
mod installer;
mod logging;
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

use config::Config;
use installer::{needs_setup, reconcile_autostart, run_wizard_gui, AutostartConfig};
use sync::{create_engine_channels, EngineCommand, SyncEngine};

/// Main entry point, runs tray on main thread (required for macOS)
fn main() -> Result<()> {
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

    let force_setup = args.iter().any(|a| a == "--setup" || a == "-s");
    let missing_permissions = !installer::all_permissions_granted();

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

        // Use Unix exec to replace this process with a fresh one
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&exe).args(&filtered_args).exec();
            // exec() only returns on error
            error!("exec failed: {}", err);
        }

        // Fallback for non-Unix or if exec fails
        std::process::Command::new(&exe)
            .args(&filtered_args)
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

    // Prime the capture mode + target list before initialize so the canvas can choose the
    // multi-monitor per-app envelope vs the display-capture canvas (setup_capture re-sets these).
    capture_ctx.set_single_active_app_capture(config.capture.single_active_app_capture);
    let target_apps = config.capture.target_apps.clone();
    capture_ctx.set_target_apps(&target_apps);

    // Initialize libobs context
    if let Err(e) = capture_ctx.initialize() {
        error!("Failed to initialize libobs: {}", e);
        std::process::exit(1);
    }
    info!("libobs context initialized");

    // Set up capture sources (per-app window capture for target apps, or display capture).
    if let Err(e) = capture_ctx.setup_capture(&target_apps, &config.capture.restore_tokens) {
        error!("Failed to setup capture: {}", e);
        std::process::exit(1);
    }
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

        // Wait for engine thread to finish
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
    println!("For more information, visit: https://github.com/crowd-cast/crowd-cast");
}
