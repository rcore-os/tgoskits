//! User address space management and user-space memory access.

mod access;
mod aspace;
mod io;
mod layout;
mod loader;
mod stats;
mod vm_stat;

pub use self::{access::*, aspace::*, io::*, layout::*, loader::*, stats::*, vm_stat::*};

#[cfg(feature = "uaccess-lock-regression")]
mod uaccess_lock_regression;
#[cfg(feature = "uaccess-lock-regression")]
pub(crate) use uaccess_lock_regression::*;
