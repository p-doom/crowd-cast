//! Installer and setup wizard components for CrowdCast
//!
//! This module handles:
//! - OBS detection and installation
//! - Plugin installation
//! - Profile configuration
//! - Application selection for capture
//! - OS permission requests
//! - Autostart setup
//! - First-run setup wizard

pub mod obs_detector;
pub mod plugin_install;
pub mod profile;
pub mod app_selector;
pub mod permissions;
pub mod autostart;
pub mod obs_websocket;
pub mod wizard;

pub use obs_detector::*;
pub use plugin_install::*;
pub use profile::*;
pub use app_selector::*;
pub use permissions::*;
pub use autostart::*;
pub use obs_websocket::*;
pub use wizard::*;
