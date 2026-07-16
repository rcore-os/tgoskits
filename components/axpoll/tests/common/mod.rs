use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

struct TestLockRuntime;

impl_trait! {
    impl LockRuntime for TestLockRuntime {
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
