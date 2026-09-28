// === 采集 / 输出参数 ===

use bitflags::bitflags;

use crate::interface::{BufType, Fract};

bitflags! {
    /// 流参数能力标志。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StreamParmCap: u32 {
        /// `timeperframe` 字段有效。
        const TIMEPERFRAME = 0x1000;
    }

    /// 流参数模式标志。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StreamParmMode: u32 {
        /// 高质量模式。
        const HIGHQUALITY = 0x0001;
    }
}

/// 采集参数
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CaptureParm {
    pub capability: StreamParmCap,   // [out] 支持的模式
    pub capturemode: StreamParmMode, // [inout] 当前模式
    pub timeperframe: Fract,         // [inout] 每帧时间（秒）
    pub extendedmode: u32,           // [inout] 驱动特有扩展
    pub readbuffers: u32,            // [out] 用于读取的缓冲区数量
    pub reserved: [u32; 4],
}

/// 输出参数
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OutputParm {
    pub capability: StreamParmCap,  // [out] 支持的模式
    pub outputmode: StreamParmMode, // [inout] 当前模式
    pub timeperframe: Fract,        // [inout] 每帧时间（秒）
    pub extendedmode: u32,          // [inout] 驱动特有扩展
    pub writebuffers: u32,          // [out] 用于写入的缓冲区数量
    pub reserved: [u32; 4],
}

/// 流参数（与类型相关）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StreamParm {
    pub ty: BufType,           // [in] 缓冲区类型
    pub parm: StreamParmUnion, // [inout] 流参数
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union StreamParmUnion {
    pub capture: CaptureParm,
    pub output: OutputParm,
    pub raw_data: [u8; 200],
}
