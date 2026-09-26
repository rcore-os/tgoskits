//! 颜色计量控件（`V4L2_CTRL_CLASS_COLORIMETRY = 0x00a50000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_COLORIMETRY` —— 颜色计量控件。
pub const CLASS_ID: u32 = CtrlClass::Colorimetry as u32;

/// `V4L2_CID_COLORIMETRY_CLASS = (V4L2_CTRL_CLASS_COLORIMETRY | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_COLORIMETRY_CLASS_BASE = (V4L2_CTRL_CLASS_COLORIMETRY | 0x900) = 0x00a50900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

// ── HDR10 常量 ─────────────────────────────────────────────

pub const HDR10_MASTERING_PRIMARIES_X_LOW: u32 = 5;
pub const HDR10_MASTERING_PRIMARIES_X_HIGH: u32 = 37000;
pub const HDR10_MASTERING_PRIMARIES_Y_LOW: u32 = 5;
pub const HDR10_MASTERING_PRIMARIES_Y_HIGH: u32 = 42000;
pub const HDR10_MASTERING_WHITE_POINT_X_LOW: u32 = 5;
pub const HDR10_MASTERING_WHITE_POINT_X_HIGH: u32 = 37000;
pub const HDR10_MASTERING_WHITE_POINT_Y_LOW: u32 = 5;
pub const HDR10_MASTERING_WHITE_POINT_Y_HIGH: u32 = 42000;
pub const HDR10_MASTERING_MAX_LUMA_LOW: u32 = 50000;
pub const HDR10_MASTERING_MAX_LUMA_HIGH: u32 = 100000000;
pub const HDR10_MASTERING_MIN_LUMA_LOW: u32 = 1;
pub const HDR10_MASTERING_MIN_LUMA_HIGH: u32 = 50000;

// ── 复合控件结构体 ────────────────────────────────────────

/// `struct v4l2_ctrl_hdr10_cll_info` —— HDR10 内容亮度级别信息。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHdr10CllInfo {
    pub max_content_light_level: u16,
    pub max_pic_average_light_level: u16,
}

/// `struct v4l2_ctrl_hdr10_mastering_display` —— HDR10 母版显示信息。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtrlHdr10MasteringDisplay {
    pub display_primaries_x: [u16; 3],
    pub display_primaries_y: [u16; 3],
    pub white_point_x: u16,
    pub white_point_y: u16,
    pub max_display_mastering_luminance: u32,
    pub min_display_mastering_luminance: u32,
}

/// V4L2 颜色计量类控制 ID（`V4L2_CID_COLORIMETRY_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorimetryClassCtrl {
    Hdr10CllInfo = CID_BASE,
    Hdr10MasteringDisplay = CID_BASE + 1,
}
