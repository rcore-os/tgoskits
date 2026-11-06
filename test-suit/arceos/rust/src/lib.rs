#![cfg_attr(
    all(feature = "virtio-block-lifecycle", target_arch = "aarch64"),
    feature(used_with_arg)
)]

#[cfg(feature = "cpu-capacity")]
mod cpu_capacity;

#[cfg(feature = "ax-std")]
use ax_std as _;

#[cfg(all(feature = "serial-rx", target_arch = "aarch64"))]
pub mod serial_rx;

#[cfg(all(feature = "virtio-block-lifecycle", target_arch = "aarch64"))]
mod virtio_block_lifecycle;

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
    suite::SELECTED_TESTS
}

#[cfg(all(
    feature = "ax-std",
    any(feature = "debug-backtrace", feature = "debug-panic-path")
))]
pub mod debug;
#[cfg(all(feature = "display-basic", feature = "ax-std"))]
pub mod display;
#[cfg(all(feature = "ax-std", feature = "exception-breakpoint"))]
pub mod exception;
#[cfg(all(feature = "fs-basic", feature = "ax-std"))]
pub mod fs;
#[cfg(all(
    feature = "intel-itco-qemu",
    feature = "ax-std",
    target_arch = "x86_64"
))]
pub mod intel_itco;
#[cfg(all(feature = "eventfd-epoll", feature = "ax-std"))]
pub mod io_mpx;
#[cfg(all(feature = "iommu-dma", feature = "ax-std", target_arch = "aarch64"))]
pub mod iommu_dma;
#[cfg(all(
    feature = "ax-std",
    any(feature = "lockdep-baseline", feature = "lockdep-detect",)
))]
pub mod lockdep;
#[cfg(all(
    feature = "ax-std",
    any(feature = "mem-stage1-transition", feature = "memtest")
))]
pub mod mem;
#[cfg(all(
    any(feature = "net-http", feature = "net-unix-path"),
    feature = "ax-std"
))]
pub mod net;
#[cfg(all(feature = "nvme-qemu", feature = "ax-std", target_arch = "x86_64"))]
pub mod nvme;
#[cfg(all(
    feature = "ax-std",
    any(
        feature = "task-affinity",
        feature = "task-cpu-lifecycle",
        feature = "task-fair-idle-pull",
        feature = "task-fair-wake-idle-sibling",
        feature = "task-ipi",
        feature = "task-irq",
        feature = "task-kernel-timer",
        feature = "task-executor",
        feature = "task-mutex",
        feature = "task-pi-mutex",
        feature = "task-preempt-guard",
        feature = "task-priority",
        feature = "task-rt-policy",
        feature = "task-scheduler-irq-window",
        feature = "task-sleep",
        feature = "task-stack-guard-page",
        feature = "task-tls",
        feature = "task-wait-queue",
        feature = "task-wait-queue-remote-wake",
        feature = "task-yield",
    )
))]
pub mod task;

/// Feature-gated target bindings generated from the suite declaration.
pub mod suite;
