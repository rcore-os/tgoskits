#![allow(unsafe_op_in_unsafe_fn)]

mod api;
pub mod cache;
pub use axvisor_core::arch::loongarch64::inject_interrupt;

pub fn prepare_virtualization() {}
