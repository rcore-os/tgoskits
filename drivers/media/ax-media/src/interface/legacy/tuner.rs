//! 调谐器结构。

use bitflags::bitflags;

// ── 调谐器类型 ─────────────────────────────────────────────────────

/// 调谐器类型。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunerType {
    Radio     = 1,
    AnalogTv  = 2,
    DigitalTv = 3,
    Sdr       = 4,
    Rf        = 5,
}

impl TunerType {
    /// 尝试将原始 `u32` 转换为 [`TunerType`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Radio,
            2 => Self::AnalogTv,
            3 => Self::DigitalTv,
            4 => Self::Sdr,
            5 => Self::Rf,
            _ => return None,
        })
    }
}

/// 调谐器。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Tuner {
    pub index: u32,                // [in] 调谐器索引
    pub name: [u8; 32],            // [out] 名称
    pub ty: TunerType,             // [out] 调谐器类型
    pub capability: TunerCap,      // [out] 调谐器能力
    pub rangelow: u32,             // [out] 最低频率（Hz 或 kHz，取决于 LOW）
    pub rangehigh: u32,            // [out] 最高频率
    pub rxsubchans: TunerSubchans, // [out] 子信道
    pub audmode: u32,              // [in] 音频模式（V4L2_TUNER_MODE_* 值）
    pub signal: i32,               // [out] 信号强度
    pub afc: i32,                  // [out] AFC 自动频率控制
    pub reserved: [u32; 4],
}

bitflags! {
    /// 调谐器能力标志 — `V4L2_TUNER_CAP_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TunerCap: u32 {
        const LOW              = 0x0001;
        const NORM             = 0x0002;
        const HWSEEK_BOUNDED   = 0x0004;
        const HWSEEK_WRAP      = 0x0008;
        const STEREO           = 0x0010;
        const LANG2            = 0x0020;
        const SAP              = 0x0020;
        const LANG1            = 0x0040;
        const RDS              = 0x0080;
        const RDS_BLOCK_IO     = 0x0100;
        const RDS_CONTROLS     = 0x0200;
        const FREQ_BANDS       = 0x0400;
        const HWSEEK_PROG_LIM  = 0x0800;
        const HZ               = 0x1000;
    }
}

bitflags! {
    /// 子信道标志 — `V4L2_TUNER_SUB_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct TunerSubchans: u32 {
        const MONO   = 0x0001;
        const STEREO = 0x0002;
        const LANG2  = 0x0004;
        const SAP    = 0x0004;
        const LANG1  = 0x0008;
        const RDS    = 0x0010;
    }
}

/// 音频模式值 — `V4L2_TUNER_MODE_*`（值有重叠，仅作常量说明用）。
#[allow(non_upper_case_globals)]
pub mod tuner_mode {
    pub const MONO: u32 = 0x0000;
    pub const STEREO: u32 = 0x0001;
    pub const LANG2: u32 = 0x0002;
    pub const SAP: u32 = 0x0002;
    pub const LANG1: u32 = 0x0003;
    pub const LANG1_LANG2: u32 = 0x0004;
}

/// 频率。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Frequency {
    pub tuner: u32,     // [in] 调谐器索引
    pub ty: TunerType,  // [in] 调谐器类型
    pub frequency: u32, // [inout] 频率（Hz 或 kHz）
    pub reserved: [u32; 8],
}

bitflags! {
    /// 频段调制方式 — `V4L2_BAND_MODULATION_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BandModulation: u32 {
        const VSB = 1 << 1;
        const FM  = 1 << 2;
        const AM  = 1 << 3;
    }
}

/// 频率频段。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FrequencyBand {
    pub tuner: u32,                 // [in] 调谐器索引
    pub ty: TunerType,              // [in] 调谐器类型
    pub index: u32,                 // [in] 频段索引
    pub capability: TunerCap,       // [out] 频段能力
    pub rangelow: u32,              // [out] 最低频率
    pub rangehigh: u32,             // [out] 最高频率
    pub modulation: BandModulation, // [out] 调制方式
    pub reserved: [u32; 9],
}

/// 硬件频率搜索请求。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HwFreqSeek {
    pub tuner: u32,       // [in] 调谐器索引
    pub ty: TunerType,    // [in] 调谐器类型
    pub seek_upward: u32, // [in] 向上搜索（否则向下）
    pub wrap_around: u32, // [in] 到达边界后回绕
    pub spacing: u32,     // [in] 搜索步进
    pub rangelow: u32,    // [in] 最低频率限制
    pub rangehigh: u32,   // [in] 最高频率限制
    pub reserved: [u32; 5],
}
