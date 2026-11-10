mod args;
mod assets;
pub(crate) mod board;
mod c_suite;
pub(crate) mod cpu;
mod directory;
mod discovery;
mod drivers;
mod listing;
mod pcm_waveform;
mod qemu;
mod runner;
mod rust_suite;
mod serial_rx;
mod types;

pub use args::{ArgsTest, ArgsTestBoard, ArgsTestQemu, TestCommand};

use crate::arceos::ArceOS;

const ARCEOS_RUST_TEST_GROUP: &str = "rust";
const ARCEOS_C_TEST_GROUP: &str = "c";
const ARCEOS_AXTEST_GROUP: &str = "axtest";
const ARCEOS_TEST_SUITE_OS: &str = "arceos";
const ARCEOS_RUST_TEST_PACKAGE: &str = "arceos-test-suit";
const ARCEOS_RUST_TEST_BUILD_GROUP: &str = "arceos-test-suit";
const ARCEOS_C_TEST_BUILD_GROUP: &str = "arceos-c-test-suit";
const ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE: &str = "debug-panic-path";
const ARCEOS_RUST_LOCKDEP_DETECT_FEATURE: &str = "lockdep-detect";
const ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE: &str = "task-stack-guard-page";

pub(crate) async fn test(arceos: &mut ArceOS, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => runner::test_qemu(arceos, args).await,
        TestCommand::Board(args) => arceos.test_board(args).await,
    }
}

/// Runtime machine restrictions are declared with the suite metadata.
fn rust_qemu_feature_supports_arch(
    catalog: &rust_suite::SuiteCatalog,
    feature: &str,
    arch: &str,
) -> bool {
    catalog.supports_selection(feature, arch)
}
