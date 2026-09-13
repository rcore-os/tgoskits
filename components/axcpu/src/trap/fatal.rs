//! Terminal CPU faults handed directly to the owning runtime.

use core::fmt;

/// Runtime termination service, independent of Rust panic hooks and unwinding.
///
/// The caller may have interrupted an atomic section or a guest transition.
/// The implementation must not allocate, sleep, access task-local state, or
/// unwind. It must bound recursive/concurrent faults and terminate the system.
/// Only the supplied formatting arguments are known to be readable; do not
/// walk an interrupted stack or dereference addresses contained in the record.
#[trait_ffi::def_extern_trait(mod_path = "trap::fatal")]
pub trait FatalTrap {
    /// Emits a best-effort diagnostic and never resumes the faulting context.
    fn terminate(record: fmt::Arguments<'_>) -> !;
}

/// Hands an unrecoverable CPU exception to the final runtime's fatal policy.
pub fn terminate(record: fmt::Arguments<'_>) -> ! {
    fatal_trap::terminate(record)
}
