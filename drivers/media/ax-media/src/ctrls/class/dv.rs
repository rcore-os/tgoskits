//! 数字视频控件（`V4L2_CTRL_CLASS_DV = 0x00a00000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_DV` —— 数字视频控件。
pub const CLASS_ID: u32 = CtrlClass::Dv as u32;

/// `V4L2_CID_DV_CLASS = (V4L2_CTRL_CLASS_DV | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_DV_CLASS_BASE = (V4L2_CTRL_CLASS_DV | 0x900) = 0x00a00900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `enum v4l2_dv_tx_mode` —— `V4L2_CID_DV_TX_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvTxMode {
    DviD = 0,
    Hdmi = 1,
}

/// `enum v4l2_dv_rgb_range` —— `V4L2_CID_DV_TX_RGB_RANGE` / `V4L2_CID_DV_RX_RGB_RANGE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvRgbRange {
    Auto    = 0,
    Limited = 1,
    Full    = 2,
}

/// `enum v4l2_dv_it_content_type` —— `V4L2_CID_DV_TX_IT_CONTENT_TYPE` / `V4L2_CID_DV_RX_IT_CONTENT_TYPE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvItContentType {
    Graphics = 0,
    Photo    = 1,
    Cinema   = 2,
    Game     = 3,
    NoItc    = 4,
}

/// V4L2 数字视频类控制 ID（`V4L2_CID_DV_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvClassCtrl {
    TxHotplug       = CID_BASE + 1,
    TxRxsense       = CID_BASE + 2,
    TxEdidPresent   = CID_BASE + 3,
    TxMode          = CID_BASE + 4,
    TxRgbRange      = CID_BASE + 5,
    TxItContentType = CID_BASE + 6,
    RxPowerPresent  = CID_BASE + 100,
    RxRgbRange      = CID_BASE + 101,
    RxItContentType = CID_BASE + 102,
}
