#![no_std]

extern crate alloc;

mod fs;
mod mount;
mod node;
pub mod path;
mod types;

pub use fs::*;
pub use mount::*;
pub use node::*;
pub use types::*;

pub type VfsError = axerrno::AxError;
pub type VfsResult<T> = Result<T, VfsError>;

// VFS operations should be atomic.
use kspin::{SpinNoPreempt as Mutex, SpinNoPreemptGuard as MutexGuard};
