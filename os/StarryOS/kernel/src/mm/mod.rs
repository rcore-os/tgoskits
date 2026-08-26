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
    record_eager_user_memory_preparation, record_faulting_user_copy, record_user_copy_completed,
    synchronize_user_copy_with_address_space_holder,
};
pub use self::{access::*, aspace::*, io::*, loader::*, stats::*, vm_stat::*};
