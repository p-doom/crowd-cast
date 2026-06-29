//! Setup utilities for crowd-cast
//!
//! This module handles:
//! - Setup wizard for application selection (CLI and GUI)
//! - OS permission requests (Screen Recording, Accessibility)
//! - Autostart setup

pub mod autostart;
pub mod permissions;
pub mod wizard;
pub mod wizard_ffi;
pub mod wizard_gui;
#[cfg(target_os = "windows")]
mod wizard_windows;
#[cfg(target_os = "windows")]
pub use wizard_windows::{run_settings_panel, AppPickerResult};

#[cfg(target_os = "linux")]
pub mod requirements;

#[cfg(target_os = "linux")]
pub mod gnome_focus;

pub use autostart::*;
pub use permissions::*;
pub use wizard::{needs_setup, run_wizard, run_wizard_async, WizardResult};
pub use wizard_gui::{run_wizard_gui, WizardResult as GuiWizardResult};
