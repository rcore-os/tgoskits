//! AIC8800 固件地址和 Patch 配置常量
//!
//! 共享常量已迁移到 aic8800_common crate。
//! 此文件仅保留固件加载专用常量。

// Re-export 所有共享常量和类型
pub use crate::common::*;

// ============================================================
// 固件 RAM 地址（固件加载专用）
// ============================================================
/// WiFi FMAC 固件加载地址 (AIC8801/D80/D80X2)
pub const RAM_FMAC_FW_ADDR: u32 = 0x0012_0000;
pub const ROM_FMAC_FW_ADDR: u32 = 0x0001_0000;
/// WiFi FMAC 固件补丁地址 (AIC8801)
pub const RAM_FMAC_FW_PATCH_ADDR: u32 = 0x0019_0000;
/// ROM FMAC 固件补丁地址 (AIC8800DC)
pub const ROM_FMAC_PATCH_ADDR: u32 = 0x0018_0000;
/// ROM FMAC 校准固件加载地址 (AIC8800DC DPD calib)
pub const ROM_FMAC_CALIB_ADDR: u32 = 0x0013_0000;

// ============================================================
// 固件上传常量
// ============================================================
/// 固件上传块大小
pub const FW_UPLOAD_CHUNK_SIZE: usize = 1024;
/// 固件上传进度打印间隔
pub const FW_UPLOAD_PROGRESS_INTERVAL: usize = 65536;
/// 固件 config_base 偏移 (RAM_FMAC_FW_ADDR + 0x180)
pub const FW_CONFIG_BASE_OFFSET: u32 = 0x0180;

// ============================================================
// Patch 配置地址（固件加载专用）
// ============================================================
/// Patch 地址寄存器 (NORMAL mode)
pub const PATCH_ADDR_REG: u32 = 0x001e_5318;
/// Patch 数量寄存器 (NORMAL mode)
pub const PATCH_NUM_REG: u32 = 0x001e_531c;
/// Patch 表起始地址
pub const PATCH_TBL_START_ADDR: u32 = 0x001e_6000;

/// patch_tbl
pub const PATCH_TBL: &[[u32; 2]] = &[
    [0x0104, 0x0000_0000], // link_det_5g disabled
];
/// syscfg_tbl_masked: {addr, mask, data}
pub const SYSCFG_TBL_MASKED: &[[u32; 3]] = &[
    [0x4050_6024, 0x0000_00FF, 0x0000_00DF], // clock gate lp_level
];
/// rf_tbl_masked: {addr, mask, data}
pub const RF_TBL_MASKED: &[[u32; 3]] = &[
    [0x4034_4058, 0x0080_0000, 0x0000_0000], // PLL TRX
];
pub const SYSCFG_TBL: &[(u32, u32)] = &[
    (0x40500014, 0x00000101),
    (0x40500018, 0x00000109),
    (0x40500004, 0x00000010),
    (0x40040000, 0x00001AC8), // fix panic
    (0x40040084, 0x00011580),
    (0x40040080, 0x00000001),
    (0x40100058, 0x00000000),
    (0x50000000, 0x03220204), // PMIC init
    (0x50019150, 0x00000002), // 26MHz xtal div
    (0x50017008, 0x00000000), // STOP WATCHDOG
];
