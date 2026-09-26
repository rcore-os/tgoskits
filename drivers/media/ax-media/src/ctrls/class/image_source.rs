//! 图像源控件（`V4L2_CTRL_CLASS_IMAGE_SOURCE = 0x009e0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_IMAGE_SOURCE` —— 图像源控件。
pub const CLASS_ID: u32 = CtrlClass::ImageSource as u32;

/// `V4L2_CID_IMAGE_SOURCE_CLASS = (V4L2_CTRL_CLASS_IMAGE_SOURCE | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_IMAGE_SOURCE_CLASS_BASE = (V4L2_CTRL_CLASS_IMAGE_SOURCE | 0x900) = 0x009e0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// V4L2 图像源类控制 ID（`V4L2_CID_IMAGE_SOURCE_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageSourceClassCtrl {
    Vblank            = CID_BASE + 1,
    Hblank            = CID_BASE + 2,
    AnalogueGain      = CID_BASE + 3,
    TestPatternRed    = CID_BASE + 4,
    TestPatternGreenR = CID_BASE + 5,
    TestPatternBlue   = CID_BASE + 6,
    TestPatternGreenB = CID_BASE + 7,
    UnitCellSize      = CID_BASE + 8,
    NotifyGains       = CID_BASE + 9,
}
