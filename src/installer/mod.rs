//! Setup utilities for crowd-cast
//!
//! This module handles:
//! - Setup wizard for application selection (CLI and GUI)
//! - OS permission requests (Screen Recording, Accessibility)
//! - Autostart setup

pub mod permissions;
pub mod autostart;
pub mod wizard;
pub mod wizard_ffi;
pub mod wizard_gui;

pub use permissions::*;
pub use autostart::*;
pub use wizard::{run_wizard, run_wizard_async, needs_setup, WizardResult};
pub use wizard_gui::{run_wizard_gui, WizardResult as GuiWizardResult};
