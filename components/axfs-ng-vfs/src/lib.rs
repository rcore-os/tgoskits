#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for VFS helpers and node contracts.
pub mod axtest;

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

pub type Mutex<T> = ax_kspin::SpinNoPreempt<T>;
pub type MutexGuard<'a, T> = ax_kspin::SpinNoPreemptGuard<'a, T>;
