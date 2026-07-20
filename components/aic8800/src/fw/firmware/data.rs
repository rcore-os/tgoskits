//! 固件二进制数据 — 编译时嵌入
//!
//! 所有固件文件通过 include_bytes!() 在编译时嵌入内核镜像。
//! 固件 blob 由 `build.rs` 准备到 `OUT_DIR/firmware/`（来源优先级：
//! `$AIC8800_FIRMWARE_DIR` → 仓库内 `firmware/`（可选本地缓存）→ 上游下载），
//! 每个文件均按 SHA-256 校验，因此 blob 不必随源码/发布包分发。
//!
//! 运行时通过 get_firmware_set() 根据芯片型号和版本选择正确的固件组合。

use super::super::chip::{ChipRevision, ChipVariant};

/// `build.rs` 写入固件 blob 的目录（`OUT_DIR/firmware`）。
macro_rules! fw_path {
    ($name:literal) => {
        concat!(env!("OUT_DIR"), "/firmware/", $name)
    };
}

// ============================================================
// AIC8801 固件 (U02/U03/U04 通用)
// ============================================================

/// AIC8801
pub static FW_8801: &[u8] = include_bytes!(fw_path!("fmacfw.bin"));
pub static FW_8801_PATCH: &[u8] = include_bytes!(fw_path!("fmacfw_patch.bin"));
pub static FW_8801_PATCH_TBL: &[u8] = &[];

// ============================================================
// AIC8800DC 固件
// ============================================================

/// AIC8800DC U01/U02/H U02
pub static FW_DC: &[u8] = include_bytes!(fw_path!("fmacfw_patch_8800dc_u02.bin"));
pub static FW_DC_PATCH: &[u8] = include_bytes!(fw_path!("fw_patch_8800dc_u02.bin"));
pub static FW_DC_PATCH_TBL: &[u8] = include_bytes!(fw_path!("fw_patch_table_8800dc_u02.bin"));
/// AIC8800DC WiFi 补丁表 (fmacfw_patch_tbl, 不同于上面的 BT patch table)
/// patch_config 阶段用它做代码跳转表 (aicwf_patch_table_load)
pub static FW_DC_FMAC_PATCH_TBL: &[u8] =
    include_bytes!(fw_path!("fmacfw_patch_tbl_8800dc_u02.bin"));
/// AIC8800DC-H (sub_id==2) 专属 patch 固件 + 跳转表 (纯 WiFi, CONFIG_SDIO_BT=n)
pub static FW_DC_H_PATCH: &[u8] = include_bytes!(fw_path!("fmacfw_patch_8800dc_h_u02.bin"));
pub static FW_DC_H_FMAC_PATCH_TBL: &[u8] =
    include_bytes!(fw_path!("fmacfw_patch_tbl_8800dc_h_u02.bin"));
/// AIC8800DC-H DPD 校准固件 (上传 0x130000, FNCALL 跑起来初始化 RF/misc RAM)
pub static FW_DC_H_CALIB: &[u8] = include_bytes!(fw_path!("fmacfw_calib_8800dc_h_u02.bin"));
/// AIC8800DC RF 配置 blob (patch_config 阶段上传到固件指定的 RAM 地址)。
/// 这些不是固件镜像,而是 vendor BSP 源码 `aic8800dc_compat.c` 的 u32 数组,
/// 无上游下载源,故内联为 Rust 字节数组(见 `dc_rf_cfg`),不再 vendor .bin。
pub use super::dc_rf_cfg::{FW_DC_AGC_CFG, FW_DC_LDPC_CFG, FW_DC_TXGAIN_MAP, FW_DC_TXGAIN_MAP_H};

// ============================================================
// AIC8800D80 固件
// ============================================================

/// AIC8800D80 U01/U02/H U02 固件
pub static FW_D80: &[u8] = include_bytes!(fw_path!("fmacfw_8800d80_u02.bin"));
pub static FW_D80_PATCH: &[u8] = include_bytes!(fw_path!("fw_patch_8800d80_u02.bin"));
pub static FW_D80_PATCH_TBL: &[u8] = include_bytes!(fw_path!("fw_patch_table_8800d80_u02.bin"));

// ============================================================
// AIC8800D80X2 固件
// ============================================================
pub static FW_D80X2: &[u8] = include_bytes!(fw_path!("fmacfw_8800d80_u02.bin"));
pub static FW_D80X2_PATCH: &[u8] = include_bytes!(fw_path!("fw_patch_8800d80_u02.bin"));
pub static FW_D80X2_PATCH_TBL: &[u8] = include_bytes!(fw_path!("fw_patch_table_8800d80_u02.bin"));

/// 选中的固件集合
pub struct FirmwareSet {
    /// WiFi 主固件 (AIC8801/D80/D80X2) 或 补丁固件 (AIC8800DC)
    pub wl_fw: &'static [u8],
    /// 补丁表 (仅 AIC8800DC 使用, 其他芯片为空)
    pub patch_tbl: &'static [u8],
    /// AIC8801 的额外补丁固件
    pub wl_patch: &'static [u8],
    /// 描述信息
    pub desc: &'static str,
}

// ============================================================
// 运行时固件选择
// ============================================================

/// 根据芯片型号和版本信息，返回对应的固件集合
pub fn get_firmware_set(chip: ChipVariant, _rev: &ChipRevision) -> Option<FirmwareSet> {
    match chip {
        // ---- AIC8801 ----
        ChipVariant::Aic8801 => Some(FirmwareSet {
            wl_fw: FW_8801,
            patch_tbl: FW_8801_PATCH_TBL,
            wl_patch: FW_8801_PATCH,
            desc: "AIC8801 fmacfw + patch",
        }),
        // ---- AIC8800DC / AIC8800DW ----
        ChipVariant::Aic8800DC | ChipVariant::Aic8800DW => Some(FirmwareSet {
            wl_fw: FW_DC,
            patch_tbl: FW_DC_PATCH_TBL,
            wl_patch: FW_DC_PATCH,
            desc: "AIC8800DC",
        }),
        // ---- AIC8800D80 ----
        ChipVariant::Aic8800D80 => Some(FirmwareSet {
            wl_fw: FW_D80,
            patch_tbl: FW_D80_PATCH_TBL,
            wl_patch: FW_D80_PATCH,
            desc: "AIC8800D80",
        }),
        // ---- AIC8800D80X2 ----
        ChipVariant::Aic8800D80X2 => {
            log::debug!("[fw_select] AIC8800D80X2 selected");
            Some(FirmwareSet {
                wl_fw: FW_D80X2,
                patch_tbl: FW_D80X2_PATCH_TBL,
                wl_patch: FW_D80X2_PATCH,
                desc: "AIC8800D80X2",
            })
        }

        ChipVariant::Unknown => {
            log::error!("[fw_select] Unknown chip variant");
            None
        }
    }
}
