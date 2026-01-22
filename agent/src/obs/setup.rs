//! OBS auto-configuration for first-run setup
//!
//! This module provides utilities for checking OBS configuration.
//! Currently not used in the main flow but available for diagnostics.

use anyhow::{Context, Result};
use obws::Client;
use tracing::{info, warn};

/// Check if OBS is properly configured for CrowdCast
#[allow(dead_code)]
pub struct OBSSetup {
    client: Client,
}

impl OBSSetup {
    /// Create a new OBS setup helper
    pub async fn new(host: &str, port: u16, password: Option<&str>) -> Result<Self> {
        let client = Client::connect(host, port, password)
            .await
            .context("Failed to connect to OBS WebSocket")?;
        
        Ok(Self { client })
    }
    
    /// Check if the CrowdCast plugin is installed and responding
    pub async fn check_plugin_installed(&self) -> Result<bool> {
        let empty_data = serde_json::json!({});
        let result: Result<obws::responses::general::VendorResponse<serde_json::Value>, _> = self.client
            .general()
            .call_vendor_request(obws::requests::general::CallVendorRequest {
                vendor_name: "crowdcast",
                request_type: "GetHookedSources",
                request_data: &empty_data,
            })
            .await;
        
        match result {
            Ok(_) => {
                info!("CrowdCast plugin is installed and responding");
                Ok(true)
            }
            Err(e) => {
                warn!("CrowdCast plugin not found or not responding: {}", e);
                Ok(false)
            }
        }
    }
    
    /// Get the current OBS version
    pub async fn get_obs_version(&self) -> Result<String> {
        let version = self.client.general().version().await?;
        Ok(format!(
            "OBS {} (WebSocket {})",
            version.obs_version,
            version.obs_web_socket_version
        ))
    }
    
    /// Check if recording is configured with file splitting
    pub async fn check_recording_config(&self) -> Result<RecordingConfig> {
        let record_dir = self.client.config().record_directory().await.ok();
        
        Ok(RecordingConfig {
            output_path: record_dir,
            // Note: File splitting config would need additional profile parameter queries
            file_splitting_enabled: false, // Default, would need to query actual config
        })
    }
    
    /// List all window capture sources in the current scene collection
    pub async fn list_window_capture_sources(&self) -> Result<Vec<SourceInfo>> {
        let mut sources = Vec::new();
        
        // Get all inputs
        let inputs = self.client.inputs().list(None).await?;
        
        for input in inputs {
            // Check if it's a window capture type
            let is_window_capture = input.kind.contains("window") 
                || input.kind.contains("xcomposite")
                || input.kind.contains("pipewire");
            
            if is_window_capture {
                sources.push(SourceInfo {
                    name: input.id.name,
                    kind: input.kind,
                });
            }
        }
        
        Ok(sources)
    }
    
    /// Get available encoders
    pub async fn list_encoders(&self) -> Result<Vec<String>> {
        // This would require querying OBS for available encoders
        // For now, return common ones
        Ok(vec![
            "x264".to_string(),
            "nvenc".to_string(),
            "qsv".to_string(),
            "vaapi".to_string(),
            "vt_h264_hw".to_string(), // macOS VideoToolbox
        ])
    }
    
    /// Print setup status report
    pub async fn print_status_report(&self) -> Result<()> {
        println!("\n=== CrowdCast OBS Setup Status ===\n");
        
        // OBS version
        match self.get_obs_version().await {
            Ok(version) => println!("✓ OBS Version: {}", version),
            Err(e) => println!("✗ Failed to get OBS version: {}", e),
        }
        
        // Plugin status
        match self.check_plugin_installed().await {
            Ok(true) => println!("✓ CrowdCast plugin: Installed"),
            Ok(false) => println!("✗ CrowdCast plugin: Not installed or not responding"),
            Err(e) => println!("✗ CrowdCast plugin check failed: {}", e),
        }
        
        // Recording config
        match self.check_recording_config().await {
            Ok(config) => {
                if let Some(path) = config.output_path {
                    println!("✓ Recording output: {}", path);
                } else {
                    println!("? Recording output: Not configured");
                }
            }
            Err(e) => println!("✗ Recording config check failed: {}", e),
        }
        
        // Window capture sources
        match self.list_window_capture_sources().await {
            Ok(sources) => {
                if sources.is_empty() {
                    println!("? Window capture sources: None found");
                } else {
                    println!("✓ Window capture sources:");
                    for source in sources {
                        println!("    - {} ({})", source.name, source.kind);
                    }
                }
            }
            Err(e) => println!("✗ Failed to list sources: {}", e),
        }
        
        println!("\n=================================\n");
        
        Ok(())
    }
}

/// Recording configuration status
#[derive(Debug)]
pub struct RecordingConfig {
    /// Output path for recordings
    pub output_path: Option<String>,
    /// Whether file splitting is enabled
    pub file_splitting_enabled: bool,
}

/// Information about a source
#[derive(Debug)]
pub struct SourceInfo {
    /// Source name
    pub name: String,
    /// Source type/kind
    pub kind: String,
}

/// Run setup check from command line
pub async fn run_setup_check(host: &str, port: u16, password: Option<&str>) -> Result<()> {
    let setup = OBSSetup::new(host, port, password).await?;
    setup.print_status_report().await?;
    Ok(())
}
