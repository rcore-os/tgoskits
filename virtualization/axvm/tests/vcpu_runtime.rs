//! Architecture-specific checks sharing one real-kernel runtime boot.

#![cfg_attr(target_os = "none", no_std)]
#![no_main]

use ax_runtime as _;
use ax_std as _;
use axvm as _;

#[cfg(target_arch = "aarch64")]
#[path = "vcpu-runtime/aarch64.rs"]
mod aarch64;

// The RISC-V cases are registered beside the private RFENCE implementation.
#[cfg(target_arch = "riscv64")]
#[axtest::tests]
mod tests {}
#[cfg(target_arch = "aarch64")]
mod support;
