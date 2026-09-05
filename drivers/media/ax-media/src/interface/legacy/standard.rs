//! 模拟电视标准结构。

use crate::interface::{Fract, inout::StdId};

/// 视频标准描述。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Standard {
    pub index: u32,         // [in] 标准索引
    pub id: StdId,          // [out] 视频标准 ID
    pub name: [u8; 24],     // [out] 标准名称
    pub frameperiod: Fract, // [out] 帧周期（帧，非场）
    pub framelines: u32,    // [out] 每帧行数
    pub reserved: [u32; 4],
}
