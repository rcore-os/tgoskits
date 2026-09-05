//! FM 调制器控件（`V4L2_CTRL_CLASS_FM_TX = 0x009b0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_FM_TX` —— FM 调制器控件。
pub const CLASS_ID: u32 = CtrlClass::FmTx as u32;

/// `V4L2_CID_FM_TX_CLASS = (V4L2_CTRL_CLASS_FM_TX | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_FM_TX_CLASS_BASE = (V4L2_CTRL_CLASS_FM_TX | 0x900) = 0x009b0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `enum v4l2_preemphasis` —— `V4L2_CID_TUNE_PREEMPHASIS` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preemphasis {
    Disabled = 0,
    Us50     = 1,
    Us75     = 2,
}

/// V4L2 FM 调制器类控制 ID（`V4L2_CID_FM_TX_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmTxClassCtrl {
    RdsTxDeviation       = CID_BASE + 1,
    RdsTxPi              = CID_BASE + 2,
    RdsTxPty             = CID_BASE + 3,
    RdsTxPsName          = CID_BASE + 5,
    RdsTxRadioText       = CID_BASE + 6,
    RdsTxMonoStereo      = CID_BASE + 7,
    RdsTxArtificialHead  = CID_BASE + 8,
    RdsTxCompressed      = CID_BASE + 9,
    RdsTxDynamicPty      = CID_BASE + 10,
    RdsTxTrafficAnnouncement = CID_BASE + 11,
    RdsTxTrafficProgram  = CID_BASE + 12,
    RdsTxMusicSpeech     = CID_BASE + 13,
    RdsTxAltFreqsEnable  = CID_BASE + 14,
    RdsTxAltFreqs        = CID_BASE + 15,
    AudioLimiterEnabled  = CID_BASE + 64,
    AudioLimiterReleaseTime = CID_BASE + 65,
    AudioLimiterDeviation = CID_BASE + 66,
    AudioCompressionEnabled = CID_BASE + 80,
    AudioCompressionGain = CID_BASE + 81,
    AudioCompressionThreshold = CID_BASE + 82,
    AudioCompressionAttackTime = CID_BASE + 83,
    AudioCompressionReleaseTime = CID_BASE + 84,
    PilotToneEnabled     = CID_BASE + 96,
    PilotToneDeviation   = CID_BASE + 97,
    PilotToneFrequency   = CID_BASE + 98,
    TunePreemphasis      = CID_BASE + 112,
    TunePowerLevel       = CID_BASE + 113,
    TuneAntennaCapacitor = CID_BASE + 114,
}
