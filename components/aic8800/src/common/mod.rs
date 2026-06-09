//! AIC8800 WiFi 芯片共享常量和类型
//!
//! 提供被固件加载层 (aic8800_fw) 和运行时驱动层 (aic8800_fdrv) 共同使用的：
//! - SDIO 寄存器地址（V1/V3）
//! - SDIO 帧类型标识
//! - 芯片型号和版本类型
//! - 任务 ID 和 LMAC 消息计算常量
//! - 时钟和延时常量
//!
//! 此 crate 无外部依赖，可被任何 AIC8800 相关模块引用。

// ============================================================
// SDIO Vendor / Device ID
// ============================================================
pub const VID_AIC8801: u16 = 0x5449;
pub const VID_AIC8800DC: u16 = 0xc8a1;
pub const VID_AIC8800D80: u16 = 0xc8a1;
pub const VID_AIC8800D80X2: u16 = 0xc8a1;

pub const DID_AIC8801: u16 = 0x0145;
pub const DID_AIC8800DC: u16 = 0xc08d;
pub const DID_AIC8800D80: u16 = 0x0082;
pub const DID_AIC8800D80X2: u16 = 0x2082;

// ============================================================
// 芯片版本常量
// ============================================================
pub const CHIP_REV_U01: u8 = 1;
pub const CHIP_REV_U02: u8 = 3;
pub const CHIP_REV_U03: u8 = 7;
pub const CHIP_REV_U04: u8 = 7;

/// 芯片版本寄存器地址
pub const CHIP_REV_ADDR: u32 = 0x4050_0000;
/// 芯片版本号高 16 位位移量
pub const CHIP_REV_HIGH_SHIFT: u32 = 16;
/// 芯片版本号掩码 (低 6 位)
pub const CHIP_REV_MASK: u32 = 0x3F;
/// 芯片 ID 高性能标志掩码
pub const CHIP_ID_H_MASK: u32 = 0xC0;
/// 芯片 ID 高性能标志值
pub const CHIP_ID_H_VALUE: u32 = 0xC0;

// ============================================================
// SDIO 功能寄存器 — V1 (AIC8801/DC/DW)
// ============================================================
pub const SDIOWIFI_FUNC_BLOCKSIZE: u16 = 512;
pub const SDIOWIFI_BYTEMODE_LEN_REG: u32 = 0x02;
pub const SDIOWIFI_INTR_CONFIG_REG: u32 = 0x04;
pub const SDIOWIFI_SLEEP_REG: u32 = 0x05;
pub const SDIOWIFI_WR_FIFO_ADDR: u32 = 0x07;
pub const SDIOWIFI_RD_FIFO_ADDR: u32 = 0x08;
pub const SDIOWIFI_WAKEUP_REG: u32 = 0x09;
pub const SDIOWIFI_FLOW_CTRL_REG: u32 = 0x0A;
pub const SDIOWIFI_REGISTER_BLOCK: u32 = 0x0B;
pub const SDIOWIFI_BYTEMODE_ENABLE_REG: u32 = 0x11;
pub const SDIOWIFI_BLOCK_CNT_REG: u32 = 0x12;
pub const SDIOWIFI_FLOWCTRL_MASK: u8 = 0x7F;

// ============================================================
// SDIO 功能寄存器 — V3 (D80/D80X2)
// ============================================================
pub const SDIOWIFI_INTR_ENABLE_REG_V3: u32 = 0x00;
pub const SDIOWIFI_SLEEP_REG_V3: u32 = 0x01;
pub const SDIOWIFI_WAKEUP_REG_V3: u32 = 0x02;
pub const SDIOWIFI_FLOW_CTRL_Q1_REG_V3: u32 = 0x03;
pub const SDIOWIFI_MISC_INT_STATUS_REG_V3: u32 = 0x04;
pub const SDIOWIFI_BYTEMODE_LEN_REG_V3: u32 = 0x05;
pub const SDIOWIFI_BYTEMODE_LEN_MSB_REG_V3: u32 = 0x06;
pub const SDIOWIFI_BYTEMODE_ENABLE_REG_V3: u32 = 0x07;
pub const SDIOWIFI_MISC_CTRL_REG_V3: u32 = 0x08;
pub const SDIOWIFI_FLOW_CTRL_Q2_REG_V3: u32 = 0x09;
pub const SDIOWIFI_CLK_TEST_RESULT_REG_V3: u32 = 0x0A;
pub const SDIOWIFI_RD_FIFO_ADDR_V3: u32 = 0x0F;
pub const SDIOWIFI_WR_FIFO_ADDR_V3: u32 = 0x10;

/// V3 芯片唤醒写入值
pub const SDIOWIFI_V3_WAKEUP_VALUE: u8 = 0x11;
/// V3 芯片就绪标志位
pub const SDIOWIFI_V3_SLEEP_READY_BIT: u8 = 0x10;

// ============================================================
// SDIO 帧类型
// ============================================================
pub const SDIO_TYPE_DATA: u8 = 0x01;
pub const SDIO_TYPE_CFG: u8 = 0x10;
pub const SDIO_TYPE_CFG_CMD_RSP: u8 = 0x11;
pub const SDIO_TYPE_CFG_DATA_CFM: u8 = 0x12;
pub const SDIO_TYPE_CFG_PRINT: u8 = 0x13;

// ============================================================
// 任务 ID
// ============================================================
pub const TASK_DBG: u16 = 1;
pub const DRV_TASK_ID: u16 = 100;

// ============================================================
// LMAC 消息 ID 计算
// ============================================================
pub const LMAC_MSG_ID_SHIFT: u16 = 10;
pub const LMAC_FIRST_DBG: u16 = TASK_DBG << LMAC_MSG_ID_SHIFT;

// ============================================================
// Host start app 类型
// ============================================================
pub const HOST_START_APP_AUTO: u32 = 1;
pub const HOST_START_APP_CUSTOM: u32 = 2;
pub const HOST_START_APP_FNCALL: u32 = 4;
pub const HOST_START_APP_DUMMY: u32 = 5;

// ============================================================
// 时钟
// ============================================================
pub const FIRMWARE_START_CLOCK_FREQ: u32 = 400_000;
pub const DEFAULT_CLOCK_FREQ: u32 = 25_000_000;

// ============================================================
// 芯片型号
// ============================================================
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    Aic8801,
    Aic8800DC,
    Aic8800DW,
    Aic8800D80,
    Aic8800D80X2,
    Unknown,
}

impl ChipVariant {
    pub fn from_vid_did(vid: u16, did: u16) -> Self {
        match (vid, did) {
            (VID_AIC8801, DID_AIC8801) => Self::Aic8801,
            (VID_AIC8800DC, DID_AIC8800DC) => Self::Aic8800DC,
            (VID_AIC8800D80, DID_AIC8800D80) => Self::Aic8800D80,
            (VID_AIC8800D80X2, DID_AIC8800D80X2) => Self::Aic8800D80X2,
            _ => Self::Unknown,
        }
    }

    pub fn is_v3(&self) -> bool {
        matches!(self, Self::Aic8800D80 | Self::Aic8800D80X2)
    }
}

/// CRC-8 with polynomial 0x107 (X^8 + X^2 + X + 1)
/// Used by AIC8800D80/D80X2 for SDIO transport header CRC
pub fn crc8_ponl_107(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        let mut mask: u8 = 0x80;
        while mask > 0 {
            if crc & 0x80 != 0 {
                crc = crc.wrapping_shl(1) ^ 0x07;
            } else {
                crc = crc.wrapping_shl(1);
            }
            if byte & mask != 0 {
                crc ^= 0x07;
            }
            mask >>= 1;
        }
    }
    crc
}

/// 芯片修订信息
#[derive(Debug, Clone, Copy)]
pub struct ChipRevision {
    pub rev: u8,
    pub is_chip_id_h: bool,
}
