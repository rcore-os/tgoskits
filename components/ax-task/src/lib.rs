//! OS-independent task scheduling primitives.
//!
//! The crate owns no global scheduler state. Operating systems create an explicit
//! [`TaskSystem`] and one pinned [`CpuLocal`] object for every online CPU.

#![no_std]

extern crate alloc;
extern crate self as ax_task;

#[cfg(test)]
extern crate std;

mod config;
mod epoch_mpsc;
mod error;
pub mod executor;
mod facade;
pub mod inbox;
mod irq_wait;
mod lock;
mod reclaim;
pub mod runtime;
mod scheduler;
mod system;
mod task_work;
mod thread;
mod thread_start;
pub mod timer;
mod wait_queue;

pub use config::*;
pub use error::*;
pub use facade::*;
pub use irq_wait::*;
pub use scheduler::*;
pub use system::*;
pub use thread::*;
pub use thread_start::*;
pub use wait_queue::*;

#[cfg(test)]
mod test_runtime;
