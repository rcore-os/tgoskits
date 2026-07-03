/// Logical outcome produced by one test function.
///
/// This is the raw return value of the test body. The final runner status
/// may still be adjusted by attributes such as `#[should_panic]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AxTestResult {
    /// The test body finished successfully.
    Ok,
    /// The test body reported a failure.
    Failed,
}

/// Execution policy attached to a test descriptor.
///
/// This metadata is produced during test registration and consumed by the
/// runtime to decide whether to run, skip, or dispatch with a custom executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AxTestExecutionMode {
    /// Run through the default execution path.
    Standard = 0,
    /// Mark as skipped; the test body is not executed.
    Ignore   = 1,
    /// Run with a named custom executor.
    Custom   = 2,
}

/// Runtime status for one test after applying framework-level rules.
///
/// Unlike [`AxTestResult`], this value includes framework behavior such as
/// ignore handling, executor invocation errors, and `#[should_panic]` mapping.
#[derive(Debug)]
pub enum TestRunResult {
    /// Test is considered successful.
    Ok,
    /// Test failed with a human-readable reason.
    Failed(&'static str),
    /// Test was intentionally skipped.
    Ignored,
}

/// Aggregated test statistics for a full test session.
///
/// Returned by `run_tests` and typically printed in KTAP-compatible summary
/// output by the runtime.
#[derive(Debug)]
pub struct TestSummary {
    /// Number of discovered test descriptors.
    pub total: usize,
    /// Number of successful tests.
    pub passed: usize,
    /// Number of failed tests.
    pub failed: usize,
    /// Number of skipped tests.
    pub ignored: usize,
}

/// C-compatible descriptor emitted for each registered test case.
///
/// Instances are collected into a linker section and scanned by the runtime
/// at startup to enumerate all tests without dynamic registration.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AxTestDescriptor {
    /// Short test function name.
    pub name: &'static str,
    /// Module path for display and grouping.
    pub module: &'static str,
    /// Test entry function.
    pub test_fn: fn() -> AxTestResult,
    /// Named executor requested by this test. Empty means default executor.
    pub executor_name: &'static str,
    /// Whether this test is expected to fail.
    pub should_panic: bool,
    /// Skip reason for ignored tests.
    pub ignore_reason: &'static str,
    /// Execution policy for this test descriptor.
    pub execution_mode: AxTestExecutionMode,
}

impl AxTestDescriptor {
    /// Construct a new immutable test descriptor.
    ///
    /// This function is `const` so descriptors can be generated in static
    /// contexts by registration macros.
    pub const fn new(
        name: &'static str,
        module: &'static str,
        test_fn: fn() -> AxTestResult,
        executor_name: &'static str,
        should_panic: bool,
        ignore_reason: &'static str,
        execution_mode: AxTestExecutionMode,
    ) -> Self {
        Self {
            name,
            module,
            test_fn,
            executor_name,
            should_panic,
            ignore_reason,
            execution_mode,
        }
    }
}
