//! User address space management and user-space memory access.

mod access;
mod aspace;
mod io;
mod loader;
mod stats;
mod vm_stat;

pub use self::{access::*, aspace::*, io::*, loader::*, stats::*, vm_stat::*};
#[cfg(axtest)]
pub(crate) use self::{
    stats::stats_classify_and_accumulate_rules_hold_for_test,
    vm_stat::process_vm_stat_watermarks_hold_for_test,
};
