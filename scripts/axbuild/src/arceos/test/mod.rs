mod args;
mod assets;
mod axtest_qemu;
mod board;
mod c_qemu;
mod discovery;
mod generic_qemu;
mod listing;
mod runner;
mod rust_qemu;
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

const ARCEOS_RUST_ALL_FEATURE: &str = "all";
const ARCEOS_C_ALL_FEATURE: &str = "all";
const ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE: &str = "debug-backtrace";
const ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE: &str = "debug-panic-path";
const ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE: &str = "exception-page-fault";
const ARCEOS_RUST_LOCKDEP_DETECT_FEATURE: &str = "lockdep-detect";
const ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE: &str = "task-stack-guard-page";

const ARCEOS_RUST_QEMU_FEATURES: &[&str] = &[
    ARCEOS_RUST_ALL_FEATURE,
    ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
    ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE,
    "display-basic",
    "exception-breakpoint",
    ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
    "fs-basic",
    "lockdep-baseline",
    ARCEOS_RUST_LOCKDEP_DETECT_FEATURE,
    "memtest",
    "net-loopback",
    "cfs",
    "rr",
    "task-affinity",
    "task-ipi",
    "task-irq",
    "task-parallel",
    "task-priority",
    "task-sleep",
    "task-smp-online",
    ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
    "task-tls",
    "task-wait-queue",
    "task-wait-queue-remote-wake",
    "task-yield",
];

const ARCEOS_C_QEMU_FEATURES: &[&str] = &[
    ARCEOS_C_ALL_FEATURE,
    "mem",
    "pthread-basic",
    "pthread-parallel",
    "pthread-sleep",
    "pipe",
    "epoll",
    "net-http",
];
const ARCEOS_C_QEMU_LISTED_CASES: &[&str] = &[
    "mem",
    "pthread-basic",
    "pthread-parallel",
    "pthread-sleep",
    "pipe",
    "epoll",
    "net-http",
];

pub(super) async fn test(arceos: &mut ArceOS, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => runner::test_qemu(arceos, args).await,
        TestCommand::Board(args) => arceos.test_board(args).await,
    }
}
