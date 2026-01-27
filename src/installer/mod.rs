//! Setup utilities for crowd-cast
//!
//! This module handles:
//! - Setup wizard for application selection
//! - OS permission requests (Screen Recording, Accessibility)
//! - Autostart setup

pub mod permissions;
pub mod autostart;
pub mod wizard;

pub use permissions::*;
pub use autostart::*;
pub use wizard::{run_wizard, run_wizard_async, needs_setup, WizardResult};
