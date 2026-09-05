//! DV timings 结构。

use bitflags::bitflags;

use crate::interface::Fract;

/// 逐行/隔行 — `V4L2_DV_*`。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvInterlaced {
    Progressive = 0,
    Interlaced  = 1,
}

bitflags! {
    /// 同步极性标志 — `V4L2_DV_*_POS_POL`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DvPolarity: u32 {
        const VSYNC_POS_POL = 0x0000_0001;
        const HSYNC_POS_POL = 0x0000_0002;
    }
}

bitflags! {
    /// 时序标准标志 — `V4L2_DV_BT_STD_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DvStandards: u32 {
        const CEA861 = 1 << 0;
        const DMT    = 1 << 1;
        const CVT    = 1 << 2;
        const GTF    = 1 << 3;
        const SDI    = 1 << 4;
    }
}

/// BT 时序数据。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BtTimings {
    pub width: u32,               // 有效视频宽度（像素）
    pub height: u32,              // 有效视频高度（行）
    pub interlaced: DvInterlaced, // 逐行或隔行
    pub polarities: DvPolarity,   // 同步极性
    pub pixelclock: u64,          // 像素时钟（Hz）
    pub hfrontporch: u32,         // 水平前肩
    pub hsync: u32,               // 水平同步
    pub hbackporch: u32,          // 水平后肩
    pub vfrontporch: u32,         // 垂直前肩
    pub vsync: u32,               // 垂直同步
    pub vbackporch: u32,          // 垂直后肩
    pub il_vfrontporch: u32,      // 隔行场垂直前肩
    pub il_vsync: u32,            // 隔行场垂直同步
    pub il_vbackporch: u32,       // 隔行场垂直后肩
    pub standards: DvStandards,   // 支持的时序标准
    pub flags: DvPolarity,        // 时序标志
    pub picture_aspect: Fract,    // 画面宽高比
    pub cea861_vic: u8,           // CEA-861 VIC
    pub hdmi_vic: u8,             // HDMI VIC
    pub reserved: [u8; 46],
}

/// DV 时序载荷联合。
#[repr(C)]
#[derive(Clone, Copy)]
pub union DvTimingsPayload {
    pub bt: BtTimings,
    pub reserved: [u32; 32],
}

impl core::fmt::Debug for DvTimingsPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("DvTimingsPayload")
            .field(&unsafe { self.reserved })
            .finish()
    }
}

/// DV 时序。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct DvTimings {
    pub ty: u32, // [in] 时序类型（V4L2_DV_BT_656_1120 = 0）
    pub payload: DvTimingsPayload,
}

/// DV 时序类型常量。
pub const DV_BT_656_1120: u32 = 0;

/// DV 时序枚举。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EnumDvTimings {
    pub index: u32, // [in] 枚举索引
    pub pad: u32,   // [in] pad 编号（subdev 节点专用）
    pub reserved: [u32; 2],
    pub timings: DvTimings, // [out] 对应索引的时序
}

/// BT 时序能力。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct BtTimingsCap {
    pub min_width: u32,         // 最小宽度
    pub max_width: u32,         // 最大宽度
    pub min_height: u32,        // 最小高度
    pub max_height: u32,        // 最大高度
    pub min_pixelclock: u64,    // 最小像素时钟
    pub max_pixelclock: u64,    // 最大像素时钟
    pub standards: DvStandards, // 支持的时序标准
    pub capabilities: DvBtCap,  // 支持的能力
    pub reserved: [u32; 16],
}

bitflags! {
    /// DV 时序能力标志 — `V4L2_DV_BT_CAP_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DvBtCap: u32 {
        const INTERLACED       = 1 << 0;
        const PROGRESSIVE      = 1 << 1;
        const REDUCED_BLANKING = 1 << 2;
        const CUSTOM           = 1 << 3;
    }
}

/// DV 时序能力载荷联合。
#[repr(C)]
#[derive(Clone, Copy)]
pub union DvTimingsCapPayload {
    pub bt: BtTimingsCap,
    pub raw_data: [u32; 32],
}

impl core::fmt::Debug for DvTimingsCapPayload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("DvTimingsCapPayload")
            .field(&unsafe { self.raw_data })
            .finish()
    }
}

/// DV 时序能力。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DvTimingsCap {
    pub ty: u32,  // [in] 时序类型
    pub pad: u32, // [in] pad 编号（subdev 节点专用）
    pub reserved: [u32; 2],
    pub payload: DvTimingsCapPayload, // [out] 时序能力载荷
}
