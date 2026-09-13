//! OS-independent capability adapter from the AIC8800 core to RDIF Ethernet.
//!
//! The adapter owns no task, lock, sleep primitive, interrupt registration, or
//! platform discovery. It moves the physical SDIO host into a single owner
//! state machine, splits out a bounded hard-IRQ endpoint, and exchanges
//! move-only network buffers through bounded SPSC queues.

mod device;
mod error;
mod owner;

pub use device::{AicRdifDevice, AicRdifOptions};
pub use error::{AicRdifError, AicSdioIdentity};
