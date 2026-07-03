use alloc::vec::Vec;

use crate::{
    axtest_println,
    framework::{AxTestDescriptor, AxTestExecutionMode, AxTestResult, TestRunResult, TestSummary},
    print::{AxTestPrintFn, set_printer},
};

/// Abstraction for how a single test function is executed.
///
/// Different runtimes can provide their own executor strategy, such as
/// running each test in a dedicated thread, process, or inline on the
/// current context.
pub trait AxTestExecutor: Sync {
    /// A unique, human-readable executor name.
    fn name(&self) -> &'static str;

    /// Execute one test case function and return its result.
    fn run(&self, test_fn: fn() -> AxTestResult) -> Result<AxTestResult, &'static str>;
}

#[derive(Clone, Copy)]
struct AxNamedExecutor {
    pub name: &'static str,
    pub run: AxTestRunFn,
}

type AxTestRunFn = fn(fn() -> AxTestResult) -> Result<AxTestResult, &'static str>;

/// Default executor that runs the test function inline.
///
/// This is useful for minimal environments that do not support spawning
/// a separate test thread.
#[derive(Default)]
pub struct InlineExecutor;

impl AxTestExecutor for InlineExecutor {
    fn name(&self) -> &'static str {
        "inner"
    }

    fn run(&self, test_fn: fn() -> AxTestResult) -> Result<AxTestResult, &'static str> {
        Ok(test_fn())
    }
}

const MAX_EXECUTORS: usize = 16;

#[derive(Clone)]
pub struct AxTestInitBuilder {
    default_executor_name: &'static str,
    executor_registry: [Option<AxNamedExecutor>; MAX_EXECUTORS],
    executor_count: usize,
    crate_filters: Vec<&'static str>,
    printer: Option<AxTestPrintFn>,
}

/// Start executor initialization with fluent builder style.
///
/// Typical usage:
/// `axtest::init().add_executor(axtest::InlineExecutor).add_executor(ThreadedExecutor).run_tests();`
pub fn init() -> AxTestInitBuilder {
    AxTestInitBuilder {
        default_executor_name: "inner",
        executor_registry: [None; MAX_EXECUTORS],
        executor_count: 0,
        crate_filters: Vec::new(),
        printer: None,
    }
}

/// Execute a test function inline in the current context.
fn run_inline(test_fn: fn() -> AxTestResult) -> Result<AxTestResult, &'static str> {
    Ok(test_fn())
}

/// Adapt an executor type into the internal function-pointer registry entry.
fn run_with_type<E: AxTestExecutor + Default>(
    test_fn: fn() -> AxTestResult,
) -> Result<AxTestResult, &'static str> {
    E::default().run(test_fn)
}

fn module_crate_name(module: &str) -> &str {
    module.split("::").next().unwrap_or(module)
}

impl AxTestInitBuilder {
    fn matches_filter(&self, module: &str) -> bool {
        if self.crate_filters.is_empty() {
            return true;
        }

        let crate_name = module_crate_name(module);
        self.crate_filters.contains(&crate_name)
    }

    /// Read all registered test descriptors from the linker-collected `.axtest_array` section.
    fn collect_tests(&self) -> &'static [AxTestDescriptor] {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            #[link_name = "__axtest_array"]
            static _axtest_array: AxTestDescriptor;
            #[link_name = "__axtest_array_end"]
            static _axtest_array_end: AxTestDescriptor;
        }

        unsafe {
            let start = core::ptr::addr_of!(_axtest_array);
            let end = core::ptr::addr_of!(_axtest_array_end);
            if start.is_null() || end.is_null() || start >= end {
                &[]
            } else {
                core::slice::from_raw_parts(start, end.offset_from(start) as usize)
            }
        }
    }

    /// Insert or replace an executor entry in the local registry.
    fn register_executor(&mut self, entry: AxNamedExecutor) {
        // Same-name registration replaces the previous executor implementation.
        let mut idx = 0;
        while idx < self.executor_count {
            if let Some(existing) = self.executor_registry[idx]
                && existing.name == entry.name
            {
                self.executor_registry[idx] = Some(entry);
                return;
            }
            idx += 1;
        }

        if self.executor_count < MAX_EXECUTORS {
            self.executor_registry[self.executor_count] = Some(entry);
            self.executor_count += 1;
        }
    }

    /// Find a previously registered executor by name.
    fn find_executor(&self, name: &'static str) -> Option<AxNamedExecutor> {
        let mut idx = 0;
        while idx < self.executor_count {
            if let Some(entry) = self.executor_registry[idx]
                && entry.name == name
            {
                return Some(entry);
            }
            idx += 1;
        }
        None
    }

    /// Run a test function with the requested executor name.
    ///
    /// Resolution order is:
    /// 1. requested executor name (if non-empty)
    /// 2. configured default executor
    /// 3. built-in inline execution
    pub(crate) fn run_with_executor_name(
        &self,
        name: &'static str,
        test_fn: fn() -> AxTestResult,
    ) -> Result<AxTestResult, &'static str> {
        // Empty per-test executor means "use configured default".
        let selected = if name.is_empty() {
            self.default_executor_name
        } else {
            name
        };

        if let Some(entry) = self.find_executor(selected) {
            return (entry.run)(test_fn);
        }

        if let Some(entry) = self.find_executor(self.default_executor_name) {
            return (entry.run)(test_fn);
        }

        // As a final safety net, always execute inline.
        run_inline(test_fn)
    }

    /// Execute one test descriptor and map its result to test-run status.
    fn run_single_test(&self, test: &AxTestDescriptor) -> TestRunResult {
        match test.execution_mode {
            AxTestExecutionMode::Ignore => TestRunResult::Ignored,
            _ => {
                let result = match self.run_with_executor_name(test.executor_name, test.test_fn) {
                    Ok(ret) => ret,
                    Err(reason) => return TestRunResult::Failed(reason),
                };
                match (test.should_panic, result) {
                    (true, AxTestResult::Failed) => TestRunResult::Ok,
                    (true, AxTestResult::Ok) => TestRunResult::Failed(
                        "expected failure (`#[should_panic]`) but test passed",
                    ),
                    (false, AxTestResult::Ok) => TestRunResult::Ok,
                    (false, AxTestResult::Failed) => TestRunResult::Failed("test failed"),
                }
            }
        }
    }

    /// Register an executor by its intrinsic name.
    ///
    /// This API targets zero-sized, stateless executors.
    pub fn add_executor<E>(mut self, executor: E) -> Self
    where
        E: AxTestExecutor + Default,
    {
        self.register_executor(AxNamedExecutor {
            name: executor.name(),
            run: run_with_type::<E>,
        });
        self
    }

    /// Set the default executor by name.
    pub fn set_default_by_name(mut self, name: &'static str) -> Self {
        self.default_executor_name = name;
        self
    }

    /// Set the default executor using an executor value.
    pub fn set_default<E>(self, executor: E) -> Self
    where
        E: AxTestExecutor,
    {
        self.set_default_by_name(executor.name())
    }

    /// Restrict execution to tests that belong to any of the specified crates.
    ///
    /// Each crate name is matched against the first segment of `module_path!()`.
    /// For example, `with_filter(&["foo", "bar"])` selects tests whose module
    /// path starts with `foo::` / `bar::` (or is exactly `foo` / `bar`).
    ///
    /// If the list is empty, filtering is disabled.
    pub fn with_filter(mut self, crate_names: &[&'static str]) -> Self {
        self.crate_filters.clear();
        self.crate_filters.extend_from_slice(crate_names);
        self
    }

    /// Configure the formatted output function used while running tests.
    pub fn set_printer(mut self, printer: AxTestPrintFn) -> Self {
        self.printer = Some(printer);
        self
    }

    /// Finalize initialization and run all tests.
    pub fn run_tests(self) -> TestSummary {
        if let Some(printer) = self.printer {
            set_printer(printer);
        }

        // Builder state is consumed here so execution always matches the built configuration.
        let tests = self.collect_tests();
        let selected_count = tests
            .iter()
            .filter(|t| self.matches_filter(t.module))
            .count();
        let mut passed = 0;
        let mut failed = 0;
        let mut ignored = 0;

        axtest_println!("AXTEST_BEGIN total={}", selected_count);
        axtest_println!("KTAP version 1");
        axtest_println!("1..{}", selected_count);
        axtest_println!("# Running {} axtests", selected_count);
        if !self.crate_filters.is_empty() {
            axtest_println!("# Filter: {} crate(s)", self.crate_filters.len());
        }
        if tests
            .iter()
            .any(|t| self.matches_filter(t.module) && t.should_panic)
        {
            axtest_println!(
                "# NOTE: #[should_panic] is treated as expected-failure in this no_std test \
                 runtime"
            );
        }

        let mut case_no = 0;
        for test in tests.iter() {
            if !self.matches_filter(test.module) {
                continue;
            }
            case_no += 1;
            axtest_println!("# START {}::{}", test.module, test.name);
            axtest_println!("# module: {}", test.module);

            let result = self.run_single_test(test);
            match result {
                TestRunResult::Ok => {
                    passed += 1;
                    axtest_println!("ok {} {}::{}", case_no, test.module, test.name);
                    axtest_println!(
                        "AXTEST_CASE status=pass module={} name={}",
                        test.module,
                        test.name
                    );
                }
                TestRunResult::Failed(reason) => {
                    failed += 1;
                    axtest_println!(
                        "not ok {} {}::{} # {}",
                        case_no,
                        test.module,
                        test.name,
                        reason
                    );
                    axtest_println!(
                        "AXTEST_CASE status=fail module={} name={} reason={}",
                        test.module,
                        test.name,
                        reason
                    );
                }
                TestRunResult::Ignored => {
                    ignored += 1;
                    axtest_println!(
                        "ok {} {}::{} # SKIP {}",
                        case_no,
                        test.module,
                        test.name,
                        test.ignore_reason
                    );
                    axtest_println!(
                        "AXTEST_CASE status=skip module={} name={} reason={}",
                        test.module,
                        test.name,
                        test.ignore_reason
                    );
                }
            }
        }

        axtest_println!(
            "AXTEST_SUMMARY pass={} fail={} skip={} total={}",
            passed,
            failed,
            ignored,
            selected_count
        );

        TestSummary {
            total: selected_count,
            passed,
            failed,
            ignored,
        }
    }
}
