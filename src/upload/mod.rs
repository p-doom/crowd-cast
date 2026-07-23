//! S3 upload via pre-signed URLs

mod log_shipper;
mod presigned;

pub use log_shipper::LogShipper;
pub use presigned::*;
