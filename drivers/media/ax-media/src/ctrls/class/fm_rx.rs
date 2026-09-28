//! FM 接收器控件（`V4L2_CTRL_CLASS_FM_RX = 0x00a10000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_FM_RX` —— FM 接收器控件。
pub const CLASS_ID: u32 = CtrlClass::FmRx as u32;

/// `V4L2_CID_FM_RX_CLASS = (V4L2_CTRL_CLASS_FM_RX | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_FM_RX_CLASS_BASE = (V4L2_CTRL_CLASS_FM_RX | 0x900) = 0x00a10900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `enum v4l2_deemphasis` —— `V4L2_CID_TUNE_DEEMPHASIS` 菜单项。
///
/// 数值与 `v4l2_preemphasis` 一致。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deemphasis {
    Disabled = 0,
    Us50     = 1,
    Us75     = 2,
}

/// V4L2 FM 接收器类控制 ID（`V4L2_CID_FM_RX_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmRxClassCtrl {
    TuneDeemphasis      = CID_BASE + 1,
    RdsReception        = CID_BASE + 2,
    RdsRxPty            = CID_BASE + 3,
    RdsRxPsName         = CID_BASE + 4,
    RdsRxRadioText      = CID_BASE + 5,
    RdsRxTrafficAnnouncement = CID_BASE + 6,
    RdsRxTrafficProgram = CID_BASE + 7,
    RdsRxMusicSpeech    = CID_BASE + 8,
}
