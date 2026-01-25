//! Application selection for window capture sources
//!
//! This module provides functionality to:
//! - Query available windows from the OBS plugin
//! - Display a terminal-based selection UI
//! - Create window capture sources for selected applications

use anyhow::{Context, Result};
use obws::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Write};
use tracing::{debug, info};

/// Information about an available window for capture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Platform-specific window identifier
    pub id: String,
    /// Window title
    pub title: String,
    /// Application name (extracted from title)
    pub app_name: String,
    /// Whether this is a suggested application
    pub suggested: bool,
}

/// Response from GetAvailableWindows vendor request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableWindowsResponse {
    /// All available windows
    pub windows: Vec<WindowInfo>,
    /// Windows matching suggested applications
    pub suggested: Vec<WindowInfo>,
    /// Source type used for window capture
    pub source_type: Option<String>,
    /// Property name for window selection
    pub window_property: Option<String>,
}

/// Request to create capture sources
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSourcesRequest {
    /// Windows to create sources for
    pub windows: Vec<CreateSourceWindow>,
}

/// Window to create a source for
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSourceWindow {
    /// Window identifier
    pub id: String,
    /// Name for the source
    pub name: String,
}

/// Response from CreateCaptureSources vendor request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSourcesResponse {
    /// Whether all sources were created successfully
    pub success: bool,
    /// Number of sources created
    pub created_count: i64,
    /// Number of sources that failed to create
    pub failed_count: i64,
    /// Successfully created sources
    pub created: Vec<CreatedSource>,
    /// Failed sources
    pub failed: Vec<FailedSource>,
}

/// Successfully created source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedSource {
    pub name: String,
    pub id: String,
}

/// Failed source creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedSource {
    pub name: String,
    pub error: String,
}

/// Selection state for a window
#[derive(Debug, Clone)]
struct SelectionItem {
    window: WindowInfo,
    selected: bool,
    index: usize,
}

/// Query available windows from the OBS plugin
pub async fn get_available_windows(client: &Client) -> Result<AvailableWindowsResponse> {
    let empty_data = serde_json::json!({});
    
    let response = client
        .general()
        .call_vendor_request(obws::requests::general::CallVendorRequest {
            vendor_name: "crowd-cast",
            request_type: "GetAvailableWindows",
            request_data: &empty_data,
        })
        .await
        .context("Failed to call crowd-cast.GetAvailableWindows")?;
    
    let result: AvailableWindowsResponse = serde_json::from_value(response.response_data)
        .context("Failed to parse GetAvailableWindows response")?;
    
    debug!("Got {} windows ({} suggested)", result.windows.len(), result.suggested.len());
    
    Ok(result)
}

/// Create window capture sources for selected windows
pub async fn create_capture_sources(
    client: &Client,
    windows: Vec<CreateSourceWindow>,
) -> Result<CreateSourcesResponse> {
    let request = serde_json::json!({
        "windows": windows
    });
    
    let response = client
        .general()
        .call_vendor_request(obws::requests::general::CallVendorRequest {
            vendor_name: "crowd-cast",
            request_type: "CreateCaptureSources",
            request_data: &request,
        })
        .await
        .context("Failed to call crowd-cast.CreateCaptureSources")?;
    
    let result: CreateSourcesResponse = serde_json::from_value(response.response_data)
        .context("Failed to parse CreateCaptureSources response")?;
    
    info!("Created {} sources, {} failed", result.created_count, result.failed_count);
    
    Ok(result)
}

/// Display the application selection UI and return selected windows
pub fn display_selection_ui(windows: &AvailableWindowsResponse) -> Result<Vec<CreateSourceWindow>> {
    if windows.windows.is_empty() {
        println!("  [!] No windows detected. Make sure your applications are open.");
        println!("      You can add window capture sources manually in OBS later.");
        return Ok(Vec::new());
    }
    
    // Build selection items
    let mut items: Vec<SelectionItem> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut index = 1;
    
    // Add suggested windows first (pre-selected)
    for window in &windows.suggested {
        if seen_ids.contains(&window.id) {
            continue;
        }
        seen_ids.insert(window.id.clone());
        items.push(SelectionItem {
            window: window.clone(),
            selected: true,
            index,
        });
        index += 1;
    }
    
    // Add non-suggested windows
    for window in &windows.windows {
        if seen_ids.contains(&window.id) {
            continue;
        }
        seen_ids.insert(window.id.clone());
        items.push(SelectionItem {
            window: window.clone(),
            selected: false,
            index,
        });
        index += 1;
    }
    
    if items.is_empty() {
        println!("  [!] No capturable windows found.");
        return Ok(Vec::new());
    }
    
    // Display UI
    loop {
        println!();
        
        // Show suggested section
        let suggested_items: Vec<&SelectionItem> = items.iter()
            .filter(|i| i.window.suggested)
            .collect();
        
        if !suggested_items.is_empty() {
            println!("Suggested applications:");
            for item in &suggested_items {
                let check = if item.selected { "x" } else { " " };
                let title = truncate_string(&item.window.title, 50);
                println!("  [{}] {:2}. {} ({})", check, item.index, item.window.app_name, title);
            }
        }
        
        // Show other section
        let other_items: Vec<&SelectionItem> = items.iter()
            .filter(|i| !i.window.suggested)
            .collect();
        
        if !other_items.is_empty() {
            println!();
            println!("Other open windows:");
            for item in &other_items {
                let check = if item.selected { "x" } else { " " };
                let title = truncate_string(&item.window.title, 50);
                println!("  [{}] {:2}. {} ({})", check, item.index, item.window.app_name, title);
            }
        }
        
        println!();
        println!("Commands:");
        println!("  - Enter number(s) to toggle selection (e.g., '3' or '1 3 5')");
        println!("  - 'a' to select all suggested apps");
        println!("  - 'n' to select none");
        println!("  - Enter (empty) to continue with current selection");
        print!("> ");
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        
        if input.is_empty() {
            // Continue with current selection
            break;
        }
        
        if input == "a" {
            // Select all suggested
            for item in &mut items {
                if item.window.suggested {
                    item.selected = true;
                }
            }
            continue;
        }
        
        if input == "n" {
            // Deselect all
            for item in &mut items {
                item.selected = false;
            }
            continue;
        }
        
        // Parse numbers
        for part in input.split_whitespace() {
            if let Ok(num) = part.parse::<usize>() {
                if let Some(item) = items.iter_mut().find(|i| i.index == num) {
                    item.selected = !item.selected;
                    let status = if item.selected { "selected" } else { "deselected" };
                    println!("  {} {}", status, item.window.app_name);
                }
            }
        }
    }
    
    // Build result
    let selected: Vec<CreateSourceWindow> = items
        .iter()
        .filter(|i| i.selected)
        .map(|i| CreateSourceWindow {
            id: i.window.id.clone(),
            name: sanitize_source_name(&i.window.app_name),
        })
        .collect();
    
    Ok(selected)
}

/// Display selection UI in non-interactive mode (select all suggested)
pub fn select_suggested_apps(windows: &AvailableWindowsResponse) -> Vec<CreateSourceWindow> {
    windows.suggested
        .iter()
        .map(|w| CreateSourceWindow {
            id: w.id.clone(),
            name: sanitize_source_name(&w.app_name),
        })
        .collect()
}

/// Truncate a string to a maximum length
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Sanitize an app name to be a valid OBS source name
fn sanitize_source_name(name: &str) -> String {
    // Remove or replace invalid characters
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    
    // Trim and ensure non-empty
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "Window Capture".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_sanitize_source_name() {
        assert_eq!(sanitize_source_name("Firefox"), "Firefox");
        assert_eq!(sanitize_source_name("VS Code"), "VS Code");
        assert_eq!(sanitize_source_name("App (v1.0)"), "App _v1_0_");
        assert_eq!(sanitize_source_name(""), "Window Capture");
    }
    
    #[test]
    fn test_truncate_string() {
        assert_eq!(truncate_string("short", 10), "short");
        assert_eq!(truncate_string("this is a very long string", 15), "this is a ve...");
    }
}
