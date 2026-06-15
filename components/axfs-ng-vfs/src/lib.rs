#![no_std]

extern crate alloc;

mod fs;
mod mount;
mod node;
pub mod path;
mod poll;
mod types;

pub use fs::*;
pub use mount::*;
pub use node::*;
pub use poll::*;
pub use types::*;

pub type VfsError = ax_errno::AxError;
pub type VfsResult<T> = Result<T, VfsError>;

use spin::Mutex;
pub type MutexGuard<'a, T> = spin::MutexGuard<'a, T, spin::Spin>;
