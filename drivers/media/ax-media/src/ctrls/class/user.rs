//! 用户类控件（`V4L2_CTRL_CLASS_USER = 0x00980000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_USER` —— 旧式 ‘user’ 控件。
pub const CLASS_ID: u32 = CtrlClass::User as u32;

/// `V4L2_CID_USER_CLASS = (V4L2_CTRL_CLASS_USER | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_BASE = (V4L2_CTRL_CLASS_USER | 0x900) = 0x00980900`。
///
/// 别名：`V4L2_CID_USER_BASE`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `V4L2_CID_LASTP1 = (V4L2_CID_BASE + 44)` —— 最后一个用户类 CID + 1。
pub const LASTP1: u32 = CID_BASE + 44;

// ── 菜单枚举 ─────────────────────────────────────────────────

/// `enum v4l2_power_line_frequency` —— `V4L2_CID_POWER_LINE_FREQUENCY` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerLineFrequency {
    Disabled = 0,
    Hz50     = 1,
    Hz60     = 2,
    Auto     = 3,
}

/// `enum v4l2_colorfx` —— `V4L2_CID_COLORFX` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colorfx {
    None         = 0,
    Bw           = 1,
    Sepia        = 2,
    Negative     = 3,
    Emboss       = 4,
    Sketch       = 5,
    SkyBlue      = 6,
    GrassGreen   = 7,
    SkinWhiten   = 8,
    Vivid        = 9,
    Aqua         = 10,
    ArtFreeze    = 11,
    Silhouette   = 12,
    Solarization = 13,
    Antique      = 14,
    SetCbCr      = 15,
    SetRgb       = 16,
}

// ── 用户类控制 ID ───────────────────────────────────────────────

/// V4L2 用户类控制 ID（`V4L2_CID_BASE` + 偏移）。
///
/// 设计：`V4L2_CID_BRIGHTNESS = (V4L2_CTRL_CLASS_USER | 0x900) + 0`。
///
/// 使用 `as u32` 获取供 `CtrlHandler::find()` 使用的原始 CID。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserClassCtrl {
    Brightness           = CID_BASE,
    Contrast             = CID_BASE + 1,
    Saturation           = CID_BASE + 2,
    Hue                  = CID_BASE + 3,
    AudioVolume          = CID_BASE + 5,
    AudioBalance         = CID_BASE + 6,
    AudioBass            = CID_BASE + 7,
    AudioTreble          = CID_BASE + 8,
    AudioMute            = CID_BASE + 9,
    AudioLoudness        = CID_BASE + 10,
    BlackLevel           = CID_BASE + 11,
    AutoWhiteBalance     = CID_BASE + 12,
    DoWhiteBalance       = CID_BASE + 13,
    RedBalance           = CID_BASE + 14,
    BlueBalance          = CID_BASE + 15,
    Gamma                = CID_BASE + 16,
    Exposure             = CID_BASE + 17,
    Autogain             = CID_BASE + 18,
    Gain                 = CID_BASE + 19,
    Hflip                = CID_BASE + 20,
    Vflip                = CID_BASE + 21,
    PowerLineFrequency   = CID_BASE + 24,
    HueAuto              = CID_BASE + 25,
    WhiteBalanceTemperature = CID_BASE + 26,
    Sharpness            = CID_BASE + 27,
    BacklightCompensation = CID_BASE + 28,
    ChromaAgc            = CID_BASE + 29,
    ColorKiller          = CID_BASE + 30,
    Colorfx              = CID_BASE + 31,
    Autobrightness       = CID_BASE + 32,
    BandStopFilter       = CID_BASE + 33,
    Rotate               = CID_BASE + 34,
    BgColor              = CID_BASE + 35,
    ChromaGain           = CID_BASE + 36,
    Illuminators1        = CID_BASE + 37,
    Illuminators2        = CID_BASE + 38,
    MinBuffersForCapture = CID_BASE + 39,
    MinBuffersForOutput  = CID_BASE + 40,
    AlphaComponent       = CID_BASE + 41,
    ColorfxCbCr          = CID_BASE + 42,
    ColorfxRgb           = CID_BASE + 43,
}
