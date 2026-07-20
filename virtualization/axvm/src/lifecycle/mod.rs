//! VM lifecycle state machine.

pub mod machine;
pub mod status;

pub(crate) use machine::Machine;
pub use status::{StopReason, VmStatus};
