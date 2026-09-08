//! 视频输入与输出结构。
//!
//! Input、Output 及相关常量。

use bitflags::bitflags;

/// 视频标准 ID（64 位位掩码）。
pub type StdId = u64;

/// 视频输入。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Input {
    pub index: u32,          // [in] 哪个输入
    pub name: [u8; 32],      // [out] 标签
    pub ty: InputType,       // [out] 输入类型
    pub audioset: u32,       // [out] 关联的音频（位域）
    pub tuner: u32,          // [out] 调谐器索引
    pub std: StdId,          // [out] 支持的视频标准
    pub status: InStatus,    // [out] 输入状态标志
    pub capabilities: InCap, // [out] 输入能力
    pub reserved: [u32; 3],
}

// ── 输入类型 ───────────────────────────────────────────────────────────

/// 视频输入类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputType {
    Tuner  = 1,
    Camera = 2,
    Touch  = 3,
}

impl InputType {
    /// 尝试将原始 `u32` 转换为 [`InputType`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Tuner,
            2 => Self::Camera,
            3 => Self::Touch,
            _ => return None,
        })
    }
}

// ── 输入状态标志 ───────────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InStatus: u32 {
        const NO_POWER      = 0x0000_0001;
        const NO_SIGNAL     = 0x0000_0002;
        const NO_COLOR      = 0x0000_0004;
        const HFLIP         = 0x0000_0010;
        const VFLIP         = 0x0000_0020;
        const NO_H_LOCK     = 0x0000_0100;
        const COLOR_KILL    = 0x0000_0200;
        const NO_V_LOCK     = 0x0000_0400;
        const NO_STD_LOCK   = 0x0000_0800;
        const NO_SYNC       = 0x0001_0000;
        const NO_EQU        = 0x0002_0000;
        const NO_CARRIER    = 0x0004_0000;
        const MACROVISION   = 0x0100_0000;
        const NO_ACCESS     = 0x0200_0000;
        const VTR           = 0x0400_0000;
    }
}

// ── 输入能力标志 ───────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct InCap: u32 {
        const DV_TIMINGS  = 0x0000_0002;
        const STD         = 0x0000_0004;
        const NATIVE_SIZE = 0x0000_0008;
    }
}

/// 视频输出。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub index: u32,           // [in] 哪个输出
    pub name: [u8; 32],       // [out] 标签
    pub ty: OutputType,       // [out] 输出类型
    pub audioset: u32,        // [out] 关联的音频（位域）
    pub modulator: u32,       // [out] 关联的调制器
    pub std: StdId,           // [out] 支持的视频标准
    pub capabilities: OutCap, // [out] 输出能力
    pub reserved: [u32; 3],
}

// ── 输出类型 ──────────────────────────────────────────────────────────

/// 视频输出类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputType {
    Modulator        = 1,
    Analog           = 2,
    AnalogVgaOverlay = 3,
}

impl OutputType {
    /// 尝试将原始 `u32` 转换为 [`OutputType`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Modulator,
            2 => Self::Analog,
            3 => Self::AnalogVgaOverlay,
            _ => return None,
        })
    }
}

// ── 输出能力标志 ──────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OutCap: u32 {
        const DV_TIMINGS  = 0x0000_0002;
        const STD         = 0x0000_0004;
        const NATIVE_SIZE = 0x0000_0008;
    }
}
