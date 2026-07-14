//! Lock-runtime symbols owned by the ax-fs-ng unit-test binary.

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

struct FsTestLockRuntime;

impl_trait! {
    impl LockRuntime for FsTestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn preempt_enter() {}
        fn preempt_exit() {}
        unsafe fn preempt_exit_irq_return() {}
        fn current_thread_id() -> u64 { 0 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}
