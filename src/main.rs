//! crowd-cast Agent
//!
//! Captures paired screencast and input data for dataset collection.
//! Uses embedded libobs for single-binary distribution.

mod capture;
mod config;
mod data;
mod input;
mod installer;
mod sync;
mod ui;
mod upload;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use config::Config;
use installer::{needs_setup, run_wizard_gui};
use sync::{create_engine_channels, EngineCommand, SyncEngine};

/// Main entry point, runs tray on main thread (required for macOS)
fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("crowd-cast Agent starting...");

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    let force_setup = args.iter().any(|a| a == "--setup" || a == "-s");

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

    // Run setup wizard if needed
    if force_setup || needs_setup(&config) {
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
            let err = std::process::Command::new(&exe)
                .args(&filtered_args)
                .exec();
            // exec() only returns on error
            error!("exec failed: {}", err);
        }
        
        // Fallback for non-Unix or if exec fails
        std::process::Command::new(&exe)
            .args(&filtered_args)
            .spawn()?;
        std::process::exit(0);
    }

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
    let mut capture_ctx = match runtime.block_on(capture::CaptureContext::new(get_output_directory(&config))) {
        Ok(ctx) => ctx,
        Err(e) => {
            error!("Failed to bootstrap OBS binaries: {}", e);
            std::process::exit(1);
        }
    };
    info!("OBS binaries ready");

    // Initialize libobs context
    if let Err(e) = capture_ctx.initialize() {
        error!("Failed to initialize libobs: {}", e);
        std::process::exit(1);
    }
    info!("libobs context initialized");

    // Set up capture sources (application capture for target apps, or display capture fallback)
    let target_apps = &config.capture.target_apps;
    if let Err(e) = capture_ctx.setup_capture(target_apps) {
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

    // Create engine channels
    let (cmd_tx, cmd_rx, status_tx, _status_rx) = create_engine_channels();

    // Create sync engine
    let engine = SyncEngine::new(
        config.clone(),
        capture_ctx,
        cmd_rx,
        status_tx.clone(),
        notification_rx,
    );

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

    // Set up Ctrl+C handler that sends shutdown command
    let ctrl_c_tx = cmd_tx.clone();
    let ctrl_c_runtime = runtime.clone();
    ctrlc::set_handler(move || {
        info!("Ctrl+C received, shutting down...");
        let tx = ctrl_c_tx.clone();
        ctrl_c_runtime.spawn(async move {
            let _ = tx.send(EngineCommand::Shutdown).await;
        });
        // Also signal tray to exit
        #[cfg(not(no_tray))]
        unsafe {
            ui::tray_ffi::tray_exit();
        }
    })?;

    // Run tray on main thread
    #[cfg(not(no_tray))]
    {
        let tray_cmd_tx = cmd_tx.clone();
        let tray_status_rx = status_tx.subscribe();

        info!("Starting system tray on main thread");

        match ui::TrayApp::new(tray_cmd_tx, tray_status_rx) {
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

    // Send shutdown command to engine (in case tray exited without Ctrl+C)
    runtime.block_on(async {
        let _ = cmd_tx.send(EngineCommand::Shutdown).await;
    });

    // Wait for engine thread to finish
    let _ = engine_handle.join();

    info!("Shutdown complete");
    Ok(())
}

fn get_output_directory(config: &Config) -> std::path::PathBuf {
    config.recording.output_directory
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("crowd-cast-recordings"))
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
    println!();
    println!("ENVIRONMENT:");
    println!("    RUST_LOG      Set log level (e.g., debug, info, warn)");
    println!();
    println!("For more information, visit: https://github.com/crowd-cast/crowd-cast");
}
