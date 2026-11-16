#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
use crate::empty_user_table;
#[cfg(target_arch = "aarch64")]
mod fixup;
#[cfg(target_arch = "aarch64")]
mod managed;

#[cfg(target_arch = "aarch64")]
fn pin_to(cpu: usize) {
    use std::os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::ax_hal,
    };
    ax_set_current_affinity(AxCpuMask::one_shot(cpu)).unwrap();
    for _ in 0..256 {
        if ax_hal::percpu::this_cpu_id() == cpu {
            return;
        }
        std::thread::yield_now();
    }
    assert_eq!(ax_hal::percpu::this_cpu_id(), cpu);
}

/// Checks nofault recovery and tagged ARM EL0 execution through the real runtime.
pub fn run() {
    #[cfg(target_arch = "aarch64")]
    fixup::run();
    crate::kernel_access::run();
    #[cfg(target_arch = "aarch64")]
    aarch64::run();
}
