//! VM lifecycle state machine.

pub mod error;
pub mod machine;
pub mod status;

pub use error::{VmLifecycleError, VmLifecycleResult};
pub(crate) use machine::Machine;
pub use status::{StopReason, VmStatus};
