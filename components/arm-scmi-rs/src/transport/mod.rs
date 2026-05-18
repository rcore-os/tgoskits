use crate::{Shmem, err::ScmiError, protocol::Xfer};

mod smc;

pub use smc::Smc;

/// Transport-layer interface for SCMI message exchange.
///
/// Implementors provide the mechanism for sending a message through shared
/// memory and receiving the platform's response. The trait uses associated
/// constants so the generic protocol layer can reason about transport
/// capabilities at compile time.
pub trait Transport {
    /// Maximum number of outstanding messages the transport supports.
    const MAX_MSG: usize;
    /// Maximum payload size in bytes per message.
    const MAX_MSG_SIZE: usize;
    /// Whether synchronous commands are completed by the time the send call
    /// returns.
    const SYNC_CMDS_COMPLETED_ON_RET: bool;

    /// Check whether channel `idx` is available for use.
    fn chan_available(&self, idx: usize) -> bool;

    /// Return `true` when no completion interrupt is wired up and the
    /// agent must poll for completion.
    fn no_completion_irq(&self) -> bool;

    /// Send `xfer` through the transport, writing it into `shmem`.
    fn send_message(&mut self, shmem: &mut Shmem, xfer: &Xfer) -> Result<(), ScmiError>;

    /// Fetch the platform response for `xfer` from `shmem`.
    fn fetch_response(&mut self, shmem: &mut Shmem, xfer: &mut Xfer) -> Result<(), ScmiError>;
}
