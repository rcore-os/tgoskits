#![cfg_attr(all(not(test), not(feature = "std")), no_std)]
#![cfg_attr(feature = "task-tls", feature(thread_local))]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

pub type TestResult = Result<(), &'static str>;

#[derive(Clone, Copy, Debug)]
pub struct TestCase {
    pub feature: &'static str,
    pub name: &'static str,
    pub run: fn() -> TestResult,
}

impl TestCase {
    pub const fn new(feature: &'static str, name: &'static str, run: fn() -> TestResult) -> Self {
        Self { feature, name, run }
    }
}

pub fn selected_tests() -> &'static [TestCase] {
    SELECTED_TESTS
}

#[cfg(all(
    feature = "ax-std",
    any(feature = "debug-backtrace", feature = "debug-panic-path")
))]
pub mod debug;
#[cfg(all(feature = "display-basic", feature = "ax-std"))]
pub mod display;
#[cfg(all(
    feature = "ax-std",
    any(feature = "exception-breakpoint", feature = "exception-page-fault")
))]
pub mod exception;
#[cfg(all(feature = "fs-basic", feature = "ax-std"))]
pub mod fs;
#[cfg(all(
    feature = "ax-std",
    any(
        feature = "lockdep-baseline",
        feature = "lockdep-detect",
        feature = "lockdep-spin-detect"
    )
))]
pub mod lockdep;
#[cfg(all(feature = "memtest", feature = "ax-std"))]
pub mod mem;
#[cfg(all(feature = "net-loopback", feature = "ax-std"))]
pub mod net;
#[cfg(all(
    feature = "ax-std",
    any(
        feature = "sched-cfs",
        feature = "sched-rr",
        feature = "task-affinity",
        feature = "task-ipi",
        feature = "task-irq",
        feature = "task-parallel",
        feature = "task-priority",
        feature = "task-sleep",
        feature = "task-smp-online",
        feature = "task-stack-guard-page",
        feature = "task-tls",
        feature = "task-wait-queue",
        feature = "task-wait-queue-remote-wake",
        feature = "task-yield",
    )
))]
pub mod task;

macro_rules! test_runner {
    ($feature:literal, $runner:ident, $body:path) => {
        #[cfg(all(feature = $feature, feature = "ax-std"))]
        fn $runner() -> TestResult {
            $body()
        }

        #[cfg(all(feature = $feature, not(feature = "ax-std")))]
        fn $runner() -> TestResult {
            Ok(())
        }
    };
}

test_runner!(
    "debug-backtrace",
    run_debug_backtrace,
    debug::backtrace::run
);
test_runner!(
    "debug-panic-path",
    run_debug_panic_path,
    debug::panic_path::run
);
test_runner!("display-basic", run_display_basic, display::basic::run);
test_runner!(
    "exception-breakpoint",
    run_exception_breakpoint,
    exception::breakpoint::run
);
test_runner!(
    "exception-page-fault",
    run_exception_page_fault,
    exception::page_fault::run
);
test_runner!("fs-basic", run_fs_basic, fs::basic::run);
test_runner!(
    "lockdep-baseline",
    run_lockdep_baseline,
    lockdep::baseline::run
);
test_runner!("lockdep-detect", run_lockdep_detect, lockdep::detect::run);
test_runner!(
    "lockdep-spin-detect",
    run_lockdep_spin_detect,
    lockdep::spin_detect::run
);
test_runner!("memtest", run_memtest, mem::test::run);
test_runner!("net-loopback", run_net_loopback, net::loopback::run);
test_runner!("sched-cfs", run_sched_cfs, task::priority::run);
test_runner!("sched-rr", run_sched_rr, task::priority::run);
test_runner!("task-affinity", run_task_affinity, task::affinity::run);
test_runner!("task-ipi", run_task_ipi, task::ipi::run);
test_runner!("task-irq", run_task_irq, task::irq::run);
test_runner!("task-parallel", run_task_parallel, task::parallel::run);
test_runner!("task-priority", run_task_priority, task::priority::run);
test_runner!("task-sleep", run_task_sleep, task::sleep::run);
test_runner!(
    "task-smp-online",
    run_task_smp_online,
    task::smp_online::run
);
test_runner!(
    "task-stack-guard-page",
    run_task_stack_guard_page,
    task::stack_guard_page::run
);
test_runner!("task-tls", run_task_tls, task::tls::run);
test_runner!(
    "task-wait-queue",
    run_task_wait_queue,
    task::wait_queue::run
);
test_runner!(
    "task-wait-queue-remote-wake",
    run_task_wait_queue_remote_wake,
    task::wait_queue_remote_wake::run
);
test_runner!("task-yield", run_task_yield, task::yield_now::run);

const SELECTED_TESTS: &[TestCase] = &[
    #[cfg(feature = "debug-backtrace")]
    TestCase::new("debug-backtrace", "capture backtrace", run_debug_backtrace),
    #[cfg(feature = "debug-panic-path")]
    TestCase::new(
        "debug-panic-path",
        "panic backtrace path",
        run_debug_panic_path,
    ),
    #[cfg(feature = "display-basic")]
    TestCase::new(
        "display-basic",
        "draw framebuffer primitives",
        run_display_basic,
    ),
    #[cfg(feature = "exception-breakpoint")]
    TestCase::new(
        "exception-breakpoint",
        "raise breakpoint exception",
        run_exception_breakpoint,
    ),
    #[cfg(feature = "exception-page-fault")]
    TestCase::new(
        "exception-page-fault",
        "expected page fault handler",
        run_exception_page_fault,
    ),
    #[cfg(feature = "fs-basic")]
    TestCase::new("fs-basic", "bounded filesystem operations", run_fs_basic),
    #[cfg(feature = "lockdep-baseline")]
    TestCase::new(
        "lockdep-baseline",
        "lockdep baseline locking",
        run_lockdep_baseline,
    ),
    #[cfg(feature = "lockdep-detect")]
    TestCase::new(
        "lockdep-detect",
        "lockdep order inversion detection",
        run_lockdep_detect,
    ),
    #[cfg(feature = "lockdep-spin-detect")]
    TestCase::new(
        "lockdep-spin-detect",
        "lockdep spin mutex order inversion detection",
        run_lockdep_spin_detect,
    ),
    #[cfg(feature = "memtest")]
    TestCase::new("memtest", "memory allocator and collections", run_memtest),
    #[cfg(feature = "net-loopback")]
    TestCase::new(
        "net-loopback",
        "finite network address smoke",
        run_net_loopback,
    ),
    #[cfg(feature = "sched-cfs")]
    TestCase::new("sched-cfs", "CFS scheduling priority smoke", run_sched_cfs),
    #[cfg(feature = "sched-rr")]
    TestCase::new(
        "sched-rr",
        "round-robin scheduling priority smoke",
        run_sched_rr,
    ),
    #[cfg(feature = "task-affinity")]
    TestCase::new("task-affinity", "task CPU affinity", run_task_affinity),
    #[cfg(feature = "task-ipi")]
    TestCase::new("task-ipi", "IPI callback delivery", run_task_ipi),
    #[cfg(feature = "task-irq")]
    TestCase::new("task-irq", "task IRQ state", run_task_irq),
    #[cfg(feature = "task-parallel")]
    TestCase::new("task-parallel", "parallel computation", run_task_parallel),
    #[cfg(all(
        feature = "task-priority",
        not(any(feature = "sched-cfs", feature = "sched-rr"))
    ))]
    TestCase::new(
        "task-priority",
        "task priority scheduling smoke",
        run_task_priority,
    ),
    #[cfg(feature = "task-sleep")]
    TestCase::new("task-sleep", "bounded task sleeps", run_task_sleep),
    #[cfg(feature = "task-smp-online")]
    TestCase::new(
        "task-smp-online",
        "SMP online CPU and IPI readiness",
        run_task_smp_online,
    ),
    #[cfg(feature = "task-stack-guard-page")]
    TestCase::new(
        "task-stack-guard-page",
        "task stack guard page fault",
        run_task_stack_guard_page,
    ),
    #[cfg(feature = "task-tls")]
    TestCase::new("task-tls", "thread local storage", run_task_tls),
    #[cfg(feature = "task-wait-queue")]
    TestCase::new(
        "task-wait-queue",
        "wait queue wake and timeout",
        run_task_wait_queue,
    ),
    #[cfg(feature = "task-wait-queue-remote-wake")]
    TestCase::new(
        "task-wait-queue-remote-wake",
        "remote wait queue wake",
        run_task_wait_queue_remote_wake,
    ),
    #[cfg(feature = "task-yield")]
    TestCase::new("task-yield", "task yield scheduling", run_task_yield),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_tests_are_in_deterministic_order() {
        let tests = selected_tests();
        assert!(
            tests
                .windows(2)
                .all(|pair| pair[0].feature <= pair[1].feature),
            "selected test features must stay sorted for stable runner output"
        );
    }

    #[test]
    fn feature_names_are_stable_cli_names() {
        for test in selected_tests() {
            assert!(!test.feature.is_empty());
            assert!(test.feature.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
            }));
        }
    }
}
