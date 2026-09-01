//! RF 调谐器控件（`V4L2_CTRL_CLASS_RF_TUNER = 0x00a20000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_RF_TUNER` —— RF 调谐器控件。
pub const CLASS_ID: u32 = CtrlClass::RfTuner as u32;

/// `V4L2_CID_RF_TUNER_CLASS = (V4L2_CTRL_CLASS_RF_TUNER | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_RF_TUNER_CLASS_BASE = (V4L2_CTRL_CLASS_RF_TUNER | 0x900) = 0x00a20900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// V4L2 RF 调谐器类控制 ID（`V4L2_CID_RF_TUNER_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfTunerClassCtrl {
    BandwidthAuto = CID_BASE + 11,
    Bandwidth     = CID_BASE + 12,
    RfGain        = CID_BASE + 32,
    LnaGainAuto   = CID_BASE + 41,
    LnaGain       = CID_BASE + 42,
    MixerGainAuto = CID_BASE + 51,
    MixerGain     = CID_BASE + 52,
    IfGainAuto    = CID_BASE + 61,
    IfGain        = CID_BASE + 62,
    PllLock       = CID_BASE + 91,
}
