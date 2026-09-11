#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

#[cfg(target_arch = "x86_64")]
pub use ax_cpu::registers::UserXstate;
pub use ax_cpu::user::UserContext;
pub use starry_vm::{VmError, VmIo};

#[cfg(test)]
mod allocation_audit;

pub mod api;
pub mod arch;

mod error;
pub use error::{SignalError, SignalResult};

mod action;
pub use action::*;

mod pending;
pub use pending::*;

mod types;
pub use types::*;
