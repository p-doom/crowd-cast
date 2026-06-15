//! Input capture backends

mod backend;
#[cfg(not(target_os = "linux"))]
pub(crate) mod rdev_backend;
pub(crate) mod secure;

#[cfg(target_os = "linux")]
pub(crate) mod evdev_backend;

pub use backend::*;
