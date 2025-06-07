//! This crate provides a simple virtual CPU abstraction for hypervisors.
//!

#![no_std]

#[macro_use]
extern crate alloc;

mod arch_vcpu;
mod exit;
mod hal;
mod percpu;
mod vcpu;

pub use arch_vcpu::AxArchVCpu;
pub use hal::AxVCpuHal;
pub use percpu::*;
pub use vcpu::*;

pub use exit::AxVCpuExitReason;
