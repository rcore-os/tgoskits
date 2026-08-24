//! User address space management and user-space memory access.

mod access;
mod aspace;
mod io;
mod loader;
mod stats;
#[cfg(feature = "uaccess-lock-regression")]
mod uaccess_lock_regression;
mod vm_stat;

#[cfg(feature = "uaccess-lock-regression")]
pub(crate) use self::uaccess_lock_regression::{
    hold_address_space_until_user_copy, observe_user_copy_test_state,
    record_eager_user_memory_preparation, record_user_copy_completed,
};
pub use self::{access::*, aspace::*, io::*, loader::*, stats::*, vm_stat::*};
#[cfg(test)]
pub(crate) use self::{
    aspace::accounting_edge_cases_and_snapshot_rules_hold_for_test,
    aspace::accounting_rss_kind_debug_and_default_hold_for_test,
    aspace::rss_kind_and_accounting_rules_hold_for_test,
    io::vm_error_to_io_error_preserves_length_for_test,
    stats::stats_classify_and_accumulate_rules_hold_for_test,
    vm_stat::process_vm_stat_edge_cases_hold_for_test,
    vm_stat::process_vm_stat_watermarks_hold_for_test,
};
