//! Input capture backends

mod backend;
pub(crate) mod rdev_backend;

#[cfg(target_os = "linux")]
pub(crate) mod evdev_backend;

pub use backend::*;
