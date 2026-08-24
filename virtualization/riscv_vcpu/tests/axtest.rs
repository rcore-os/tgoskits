#![no_std]
#![no_main]

use ax_hal as _;
use ax_std as _;
use axtest::prelude::*;
use riscv_h::register::henvcfg;
use riscv_vcpu::{RiscvPerCpu, RiscvVcpuError};

#[axtest]
fn disabling_before_enable_preserves_host_henvcfg() {
    let previous = henvcfg::read();
    let mut per_cpu = RiscvPerCpu::new(0).expect("create per-CPU virtualization state");

    let result = per_cpu.hardware_disable();
    let observed = henvcfg::read();
    unsafe { henvcfg::write(previous) };

    ax_assert_eq!(result, Err(RiscvVcpuError::BadState));
    ax_assert_eq!(observed, previous);
}

#[axtest]
fn enabling_twice_restores_the_original_host_henvcfg() {
    let previous = henvcfg::read();
    let mut per_cpu = RiscvPerCpu::new(0).expect("create per-CPU virtualization state");
    per_cpu
        .hardware_enable()
        .expect("enable per-CPU virtualization state");

    let second_enable = per_cpu.hardware_enable();
    let disable = per_cpu.hardware_disable();
    let observed = henvcfg::read();
    unsafe { henvcfg::write(previous) };

    ax_assert_eq!(second_enable, Err(RiscvVcpuError::BadState));
    ax_assert_eq!(disable, Ok(()));
    ax_assert_eq!(observed, previous);
}

#[axtest::tests]
mod tests {}
