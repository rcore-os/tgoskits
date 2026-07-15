//! Explicit `LockRuntime` provider for host-side consumer test binaries.
//!
//! This crate is deliberately not a feature of `ax-kspin`: Cargo feature
//! unification must never replace the final kernel's runtime boundary with a
//! host implementation. Each test binary that needs context-aware locks links
//! this fixture explicitly, while binaries with their own provider omit it.

#![no_std]

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

struct HostTestLockRuntime;

impl_trait! {
    impl LockRuntime for HostTestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn preempt_enter() {}
        fn preempt_exit() {}
        unsafe fn preempt_exit_irq_return() {}
        fn current_thread_id() -> u64 { 1 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}
