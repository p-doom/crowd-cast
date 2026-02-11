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

pub use autostart::*;
pub use permissions::*;
pub use wizard::{needs_setup, run_wizard, run_wizard_async, WizardResult};
pub use wizard_gui::{run_wizard_gui, WizardResult as GuiWizardResult};
