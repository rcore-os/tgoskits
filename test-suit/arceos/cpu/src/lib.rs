//! CPU integration tests shared by normal QEMU batches and board entry points.
#![no_std]
extern crate ax_std as std;

#[cfg(all(feature = "cpufreq", target_arch = "aarch64"))]
pub mod board;
#[cfg(all(feature = "fp-context", target_arch = "aarch64"))]
pub mod fp_context;
#[cfg(feature = "guest-entry")]
pub mod guest_entry;
#[cfg(all(feature = "hypervisor-probe", target_arch = "riscv64"))]
pub mod hypervisor_probe;
#[cfg(feature = "kernel-access")]
pub mod kernel_access;
#[cfg(feature = "paging")]
pub mod paging;
#[cfg(all(feature = "pmu", target_arch = "aarch64"))]
pub mod pmu;
#[cfg(all(feature = "trap-entry", target_arch = "x86_64"))]
pub mod trap_entry;
#[cfg(all(feature = "unaligned-fixup", target_arch = "loongarch64"))]
pub mod unaligned_fixup;
#[cfg(all(feature = "user-entry", target_arch = "aarch64"))]
pub mod user_entry;
#[cfg(all(
    feature = "virtualization-lifecycle",
    any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    )
))]
pub mod virtualization_lifecycle;

#[cfg(all(
    target_arch = "x86_64",
    any(feature = "guest-entry", feature = "virtualization-lifecycle")
))]
mod support;

/// Pins a multi-CPU batch to the CPU that owns its test-local machine state.
pub fn pin_to_boot_cpu() {
    use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    for _ in 0..256 {
        if ax_hal::percpu::this_cpu_id() == 0 {
            return;
        }
        std::thread::yield_now();
    }
    assert_eq!(ax_hal::percpu::this_cpu_id(), 0);
}

#[cfg(all(feature = "kernel-access", target_arch = "aarch64"))]
mod empty_user_table;
