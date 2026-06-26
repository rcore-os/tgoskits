#![no_std]

extern crate alloc;

mod asserts;
mod coverage;
mod executor;
mod framework;
mod hooks;
pub mod print;

pub use axtest_macros::{axtest, def_mod, def_test};
pub use coverage::dump_coverage;
pub use executor::{AxTestExecutor, AxTestInitBuilder, InlineExecutor, init};
pub use framework::{
    AxTestDescriptor, AxTestExecutionMode, AxTestResult, TestRunResult, TestSummary,
};
pub use hooks::{AxTestModHookDescriptor, call_module_exit, call_module_init};
pub use print::{AxTestPrintFn, set_printer};

pub mod prelude {
    pub use crate::{AxTestResult, ax_assert, ax_assert_eq, ax_assert_ne, axtest, axtest_println};
}
