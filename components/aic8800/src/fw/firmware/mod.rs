//! 固件数据管理和上传模块

mod data;
mod upload;

pub use data::{FirmwareSet, get_firmware_set};
pub use upload::{init_aic8800d80_firmware, init_aic8800dc_firmware, init_aic8801_firmware};
