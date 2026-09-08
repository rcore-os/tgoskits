//! 相机闪光灯控件（`V4L2_CTRL_CLASS_FLASH = 0x009c0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_FLASH` —— 相机闪光灯控件。
pub const CLASS_ID: u32 = CtrlClass::Flash as u32;

/// `V4L2_CID_FLASH_CLASS = (V4L2_CTRL_CLASS_FLASH | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_FLASH_CLASS_BASE = (V4L2_CTRL_CLASS_FLASH | 0x900) = 0x009c0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

// ── 菜单枚举 ─────────────────────────────────────────────────

/// `enum v4l2_flash_led_mode` —— `V4L2_CID_FLASH_LED_MODE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashLedMode {
    None  = 0,
    Flash = 1,
    Torch = 2,
}

/// `enum v4l2_flash_strobe_source` —— `V4L2_CID_FLASH_STROBE_SOURCE` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashStrobeSource {
    Software = 0,
    External = 1,
}

// ── 位标志 ─────────────────────────────────────────────────

/// `V4L2_CID_FLASH_FAULT` 的 `V4L2_FLASH_FAULT_*` 位标志。
pub mod flash_fault {
    pub const OVER_VOLTAGE: u32 = 1 << 0;
    pub const TIMEOUT: u32 = 1 << 1;
    pub const OVER_TEMPERATURE: u32 = 1 << 2;
    pub const SHORT_CIRCUIT: u32 = 1 << 3;
    pub const OVER_CURRENT: u32 = 1 << 4;
    pub const INDICATOR: u32 = 1 << 5;
    pub const UNDER_VOLTAGE: u32 = 1 << 6;
    pub const INPUT_VOLTAGE: u32 = 1 << 7;
    pub const LED_OVER_TEMPERATURE: u32 = 1 << 8;
}

/// V4L2 相机闪光灯类控制 ID（`V4L2_CID_FLASH_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashClassCtrl {
    LedMode            = CID_BASE + 1,
    StrobeSource       = CID_BASE + 2,
    Strobe             = CID_BASE + 3,
    StrobeStop         = CID_BASE + 4,
    StrobeStatus       = CID_BASE + 5,
    Timeout            = CID_BASE + 6,
    Intensity          = CID_BASE + 7,
    TorchIntensity     = CID_BASE + 8,
    IndicatorIntensity = CID_BASE + 9,
    Fault              = CID_BASE + 10,
    Charge             = CID_BASE + 11,
    Ready              = CID_BASE + 12,
    Duration           = CID_BASE + 13,
    StrobeOe           = CID_BASE + 14,
}
