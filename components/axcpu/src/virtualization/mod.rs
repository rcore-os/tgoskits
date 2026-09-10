//! Current-target virtualization machine capabilities.

pub use crate::arch::current::virtualization::*;

mod error;
pub use error::VirtualizationError;

mod memory;
#[cfg(target_arch = "x86_64")]
pub use memory::ControlMemory;
pub use memory::{GuestPhysAddr, GuestVirtAddr};
