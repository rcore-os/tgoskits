#![no_std]
#![no_main]
extern crate ax_std as std;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
mod fixup;

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

#[unsafe(no_mangle)]
fn main() {
    fixup::run();
    aarch64::run();
    std::process::exit(0);
}
