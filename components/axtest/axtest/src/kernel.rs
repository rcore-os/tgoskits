use crate::{
    axtest_println,
    coverage::{dump_coverage, set_coverage_wait_fn},
    executor::init,
    print::AxTestPrintFn,
};

/// Runtime callbacks used by the common kernel axtest entry point.
pub struct KernelTestConfig {
    printer: AxTestPrintFn,
    shutdown: fn() -> !,
    coverage_wait: Option<fn()>,
}

impl KernelTestConfig {
    /// Create a kernel test configuration with output and shutdown callbacks.
    pub const fn new(printer: AxTestPrintFn, shutdown: fn() -> !) -> Self {
        Self {
            printer,
            shutdown,
            coverage_wait: None,
        }
    }

    /// Set the hook used to wait for host-side coverage extraction.
    pub const fn with_coverage_wait(mut self, wait: fn()) -> Self {
        self.coverage_wait = Some(wait);
        self
    }
}

/// Run all registered tests and terminate the kernel test target.
pub fn run_kernel_tests(config: KernelTestConfig) -> ! {
    if let Some(wait) = config.coverage_wait {
        set_coverage_wait_fn(wait);
    }

    let summary = init().set_printer(config.printer).run_tests();
    if summary.failed == 0 {
        dump_coverage();
        axtest_println!("AXTEST_SUITE_OK");
        (config.shutdown)();
    }

    panic!("AXTEST_SUITE_FAIL failed={}", summary.failed);
}
