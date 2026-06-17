//! TPU 字符设备 OS 适配层
//!
//! 硬件层位于 `sg2002-tpu` crate 中；本模块仅负责把 ioctl 解析到底层
//! `Sg2002Tpu` API。

mod device;
pub mod sched_probe;

pub use device::TpuDevice;
