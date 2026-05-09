//! RKNPU configuration bindings translated from the C `struct rknpu_config`.
//!
//! This module provides a `#[repr(C)]` Rust equivalent suitable for FFI
//! or direct translation of kernel-style configuration data.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum RknpuType {
    Rk3588,
}

#[derive(Debug, Clone)]
pub struct RknpuConfig {
    pub rknpu_type: RknpuType,
}
