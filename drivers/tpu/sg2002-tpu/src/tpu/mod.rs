//! TPU Driver for SG2002 (Cvitek)
//!
//! This is a no_std Rust implementation of the TPU hardware driver.
//! Ported from the original Linux kernel driver.

#![allow(dead_code)]

pub mod device;
pub mod error;
pub mod platform;
pub mod tdma;
pub mod tiu;
pub mod types;

pub use device::{Sg2002Tpu, TdmaIrqEvent, TpuState, TpuSubmitPath};
pub use platform::TiuIrqCallback;

/// TDMA 物理基地址
pub const TDMA_PHYS_BASE: usize = 0x0C10_0000;

/// TIU 物理基地址
pub const TIU_PHYS_BASE: usize = 0x0C10_1000;

/// 默认超时时间 (毫秒)
pub const DEFAULT_TIMEOUT_MS: u64 = 60 * 1000;

/// DMA buffer 魔数
pub const TPU_DMABUF_HEADER_M: u16 = 0xB5B5;
