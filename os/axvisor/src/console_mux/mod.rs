//! Reusable line and record arbitration for the host console.

mod host_log;
mod output;
mod transport;

pub use host_log::HostLogBacklog;
pub use output::{GuestOutputMux, emit_text_with_crlf};
pub use transport::{HostOutputQueue, HostOutputTransaction};
