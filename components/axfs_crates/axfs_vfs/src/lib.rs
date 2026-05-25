//! Virtual filesystem object model used by ArceOS, StarryOS, and Axvisor.
//!
//! This crate defines the shared filesystem, mount, path, node, metadata, and
//! directory-entry abstractions used by the unified `ax-fs` stack.

#![no_std]

extern crate alloc;

mod fs;
mod mount;
mod node;
mod types;

pub mod path;

use ax_errno::{AxError, AxResult};
use ax_kspin::{SpinNoIrq as Mutex, SpinNoIrqGuard as MutexGuard};

pub use self::{fs::*, mount::*, node::*, types::*};

/// Alias of [`AxError`].
pub type VfsError = AxError;

/// Alias of [`AxResult`].
pub type VfsResult<T = ()> = AxResult<T>;
