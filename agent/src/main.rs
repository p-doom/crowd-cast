//! crowd-cast Agent
//!
//! Captures paired screencast and input data for dataset collection.
//! Coordinates with OBS Studio via WebSocket and uploads to S3 via pre-signed URLs.

mod config;
mod data;
mod input;
pub mod installer;
mod obs;
mod sync;
mod ui;
mod upload;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::config::Config;
use crate::installer::{needs_setup, run_setup_wizard_async, WizardConfig};
use crate::obs::{OBSController, OBSManager, OBSManagerConfig};
use crate::sync::{EngineCommand, EngineStatus, SyncEngine};
use crate::ui::TrayApp;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("crowd-cast Agent starting...");

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    
    if args.iter().any(|a| a == "--setup" || a == "-s") {
        // Run setup wizard
        info!("Running setup wizard...");
        let config = if args.iter().any(|a| a == "--non-interactive") {
            WizardConfig {
                non_interactive: true,
                ..Default::default()
            }
        } else {
            WizardConfig::default()
        };
        
        let result = run_setup_wizard_async(config).await?;
        
        if result.success {
            info!("Setup completed successfully");
            return Ok(());
        } else {
            error!("Setup completed with errors");
            std::process::exit(1);
        }
    }
    
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return Ok(());
    }

    // Check if first-run setup is needed
    if needs_setup() {
        warn!("First-run setup required. Running setup wizard...");
        let result = run_setup_wizard_async(WizardConfig::default()).await?;
        
        if !result.success {
            error!("Setup failed. Please resolve issues and try again.");
            std::process::exit(1);
        }
    }

    // Load configuration
    let config = Config::load()?;
    info!("Configuration loaded from {:?}", config.config_path());

    // Start OBS if not already running
    let mut obs_manager = OBSManager::new(OBSManagerConfig::default())?;
    
    if !crate::installer::is_obs_running() {
        info!("Starting OBS...");
        obs_manager.launch_hidden()?;
        
        // Wait for OBS to start and websocket to be available
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    // Initialize OBS controller (connects via websocket)
    let obs = match OBSController::new(&config).await {
        Ok(obs) => obs,
        Err(e) => {
            error!("Failed to connect to OBS: {}", e);
            error!("Make sure OBS is running and WebSocket server is enabled.");
            std::process::exit(1);
        }
    };
    info!("Connected to OBS WebSocket");

    // Create channels for communication between components
    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(32);
    let (status_tx, status_rx) = broadcast::channel::<EngineStatus>(16);

    // Initialize sync engine with channels
    let sync_engine = SyncEngine::new(config.clone(), obs, obs_manager, cmd_rx, status_tx).await?;

    // Initialize tray app with channels
    let tray = match TrayApp::new(cmd_tx, status_rx) {
        Ok(tray) => tray,
        Err(e) => {
            error!("Failed to create system tray: {}", e);
            std::process::exit(1);
        }
    };

    // Run the sync engine in a background task
    let sync_handle = tokio::spawn(async move {
        if let Err(e) = sync_engine.run().await {
            error!("Sync engine error: {}", e);
        }
    });

    // Run tray UI on the main thread (blocks until exit)
    // This is required because some platforms need the tray to run on the main thread
    tray.run()?;

    // Cleanup - abort the sync engine task
    sync_handle.abort();
    
    info!("crowd-cast Agent shutting down");

    Ok(())
}

fn print_help() {
    println!("crowd-cast Agent - Paired screencast and input capture");
    println!();
    println!("USAGE:");
    println!("    crowd-cast-agent [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help            Print this help message");
    println!("    -s, --setup           Run the setup wizard");
    println!("    --non-interactive     Run setup without prompts (use defaults)");
    println!();
    println!("ENVIRONMENT:");
    println!("    RUST_LOG              Set log level (e.g., debug, info, warn)");
    println!();
    println!("For more information, visit: https://github.com/crowd-cast/crowd-cast");
}
