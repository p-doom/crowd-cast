//! Input capture backends

mod backend;
pub(crate) mod secure;
#[cfg(not(target_os = "linux"))]
pub(crate) mod rdev_backend;

#[cfg(target_os = "linux")]
pub(crate) mod evdev_backend;

pub use backend::*;
