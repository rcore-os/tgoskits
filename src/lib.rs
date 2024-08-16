//! This crate provides a simple virtual CPU abstraction for hypervisors.
//!

#![no_std]

#[macro_use]
extern crate alloc;

mod arch_vcpu;
mod exit;
mod vcpu;

pub use arch_vcpu::AxArchVCpu;
pub use vcpu::*;

// TODO: consider, should [`AccessWidth`] be moved to a new crate?
pub use exit::{AccessWidth, AxVCpuExitReason};
