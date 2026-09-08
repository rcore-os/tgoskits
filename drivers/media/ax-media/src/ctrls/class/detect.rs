//! 检测控件（`V4L2_CTRL_CLASS_DETECT = 0x00a30000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_DETECT` —— 检测控件。
pub const CLASS_ID: u32 = CtrlClass::Detect as u32;

/// `V4L2_CID_DETECT_CLASS = (V4L2_CTRL_CLASS_DETECT | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_DETECT_CLASS_BASE = (V4L2_CTRL_CLASS_DETECT | 0x900) = 0x00a30900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `enum v4l2_detect_md_mode` —— `V4L2_CID_DETECT_MD_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectMdMode {
    Disabled      = 0,
    Global        = 1,
    ThresholdGrid = 2,
    RegionGrid    = 3,
}

/// V4L2 检测类控制 ID（`V4L2_CID_DETECT_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectClassCtrl {
    Mode            = CID_BASE + 1,
    GlobalThreshold = CID_BASE + 2,
    ThresholdGrid   = CID_BASE + 3,
    RegionGrid      = CID_BASE + 4,
}
