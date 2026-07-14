use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

struct TestLockRuntime;

impl_trait! {
    impl LockRuntime for TestLockRuntime {
        fn irq_enter() {}
        fn irq_exit() {}
        fn irqs_enabled() -> bool { true }
        fn preempt_enter() {}
        fn preempt_exit() -> bool { true }
        fn in_hard_irq() -> bool { false }
        fn need_resched() -> bool { false }
        fn schedule() {}
        fn current_thread_id() -> u64 { 1 }
        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}
