//! 图像处理控件（`V4L2_CTRL_CLASS_IMAGE_PROC = 0x009f0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_IMAGE_PROC` —— 图像处理控件。
pub const CLASS_ID: u32 = CtrlClass::ImageProc as u32;

/// `V4L2_CID_IMAGE_PROC_CLASS = (V4L2_CTRL_CLASS_IMAGE_PROC | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_IMAGE_PROC_CLASS_BASE = (V4L2_CTRL_CLASS_IMAGE_PROC | 0x900) = 0x009f0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// V4L2 图像处理类控制 ID（`V4L2_CID_IMAGE_PROC_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProcClassCtrl {
    LinkFreq          = CID_BASE + 1,
    PixelRate         = CID_BASE + 2,
    TestPattern       = CID_BASE + 3,
    DeinterlacingMode = CID_BASE + 4,
    DigitalGain       = CID_BASE + 5,
}
