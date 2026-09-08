//! StarryOS 的 V4L2 核心框架（合并 videobuffer）。

#![no_std]
#[cfg(test)]
extern crate std;

extern crate alloc;

mod ctrls;
mod device;
mod driver;
mod error;
mod filehandler;
pub mod interface;
mod ioctl;
pub mod videobuffer;

pub use ctrls::{CtrlConfig, CtrlGetFn, CtrlHandler, CtrlOps, CtrlSetFn, CtrlType, class};
pub use device::VideoDevice;
pub use driver::V4L2DriverOps;
pub use error::{Result, V4l2Error};
pub use filehandler::V4l2Fh;
pub use ioctl::{IoctlCmd, IoctlOps, LegacyIoctlCmd, LegacyIoctlOps, VideoIoctl};
