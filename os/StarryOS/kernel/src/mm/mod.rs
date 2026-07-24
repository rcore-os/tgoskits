//! User address space management and user-space memory access.

mod access;
mod aspace;
mod io;
mod loader;
mod stats;

pub use starry_mm::ProcessVmStat;

pub use self::{access::*, aspace::*, io::*, loader::*, stats::*};
