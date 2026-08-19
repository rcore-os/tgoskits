//! Reusable line and record arbitration for the host console.

mod host_log;
mod output;

pub use host_log::HostLogBacklog;
pub use output::GuestOutputMux;
