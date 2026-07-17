//! SD Host Controller register offsets and bit definitions.
//!
//! Layout matches the SD Host Controller Standard Specification (v3.00 /
//! v4.00). Only the fields the MVP driver actually touches are spelled out;
//! the rest of the register file is reachable via the raw offset constants.

#![allow(dead_code)]

// ── Register offsets ────────────────────────────────────────────────────

pub(crate) const REG_SDMA_ADDR: usize = 0x00;
pub(crate) const REG_BLOCK_SIZE: usize = 0x04;
pub(crate) const REG_BLOCK_COUNT: usize = 0x06;
pub(crate) const REG_ARGUMENT: usize = 0x08;
pub(crate) const REG_TRANSFER_MODE: usize = 0x0C;
pub(crate) const REG_COMMAND: usize = 0x0E;
pub(crate) const REG_RESPONSE0: usize = 0x10;
pub(crate) const REG_RESPONSE1: usize = 0x14;
pub(crate) const REG_RESPONSE2: usize = 0x18;
pub(crate) const REG_RESPONSE3: usize = 0x1C;
pub(crate) const REG_BUFFER_DATA_PORT: usize = 0x20;
pub(crate) const REG_PRESENT_STATE: usize = 0x24;
pub(crate) const REG_HOST_CONTROL1: usize = 0x28;
pub(crate) const REG_POWER_CONTROL: usize = 0x29;
pub(crate) const REG_CLOCK_CONTROL: usize = 0x2C;
pub(crate) const REG_TIMEOUT_CONTROL: usize = 0x2E;
pub(crate) const REG_SOFTWARE_RESET: usize = 0x2F;
pub(crate) const REG_NORMAL_INT_STATUS: usize = 0x30;
pub(crate) const REG_ERROR_INT_STATUS: usize = 0x32;
pub(crate) const REG_NORMAL_INT_STATUS_ENABLE: usize = 0x34;
pub(crate) const REG_ERROR_INT_STATUS_ENABLE: usize = 0x36;
pub(crate) const REG_NORMAL_INT_SIGNAL_ENABLE: usize = 0x38;
pub(crate) const REG_ERROR_INT_SIGNAL_ENABLE: usize = 0x3A;
pub(crate) const REG_HOST_CONTROL2: usize = 0x3E;
pub(crate) const REG_CAPABILITIES_LOW: usize = 0x40;
pub(crate) const REG_CAPABILITIES_HIGH: usize = 0x44;
pub(crate) const REG_ADMA_ERROR: usize = 0x54;
pub(crate) const REG_ADMA_SYS_ADDR_LOW: usize = 0x58;
pub(crate) const REG_ADMA_SYS_ADDR_HIGH: usize = 0x5C;
pub(crate) const REG_HOST_VERSION: usize = 0xFE;

// ── Present State ──────────────────────────────────────────────────────

pub(crate) const PRESENT_CMD_INHIBIT: u32 = 1 << 0;
pub(crate) const PRESENT_DAT_INHIBIT: u32 = 1 << 1;
pub(crate) const PRESENT_BUFFER_WRITE_ENABLE: u32 = 1 << 10;
pub(crate) const PRESENT_BUFFER_READ_ENABLE: u32 = 1 << 11;
pub(crate) const PRESENT_CARD_INSERTED: u32 = 1 << 16;
pub(crate) const PRESENT_DAT0_LINE_SIGNAL_LEVEL: u32 = 1 << 20;
pub(crate) const PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL: u32 = 0x0F << 20;

// ── Software Reset ─────────────────────────────────────────────────────

pub(crate) const RESET_ALL: u8 = 1 << 0;
pub(crate) const RESET_CMD: u8 = 1 << 1;
pub(crate) const RESET_DAT: u8 = 1 << 2;

// ── Normal Interrupt Status ────────────────────────────────────────────

pub(crate) const NORMAL_INT_CMD_COMPLETE: u16 = 1 << 0;
pub(crate) const NORMAL_INT_XFER_COMPLETE: u16 = 1 << 1;
pub(crate) const NORMAL_INT_BLOCK_GAP: u16 = 1 << 2;
pub(crate) const NORMAL_INT_DMA_INTERRUPT: u16 = 1 << 3;
pub(crate) const NORMAL_INT_BUFFER_WRITE_READY: u16 = 1 << 4;
pub(crate) const NORMAL_INT_BUFFER_READ_READY: u16 = 1 << 5;
pub(crate) const NORMAL_INT_CARD_INSERTION: u16 = 1 << 6;
pub(crate) const NORMAL_INT_CARD_REMOVAL: u16 = 1 << 7;
pub(crate) const NORMAL_INT_CARD_INTERRUPT: u16 = 1 << 8;
pub(crate) const NORMAL_INT_ERROR: u16 = 1 << 15;
pub(crate) const NORMAL_INT_CLEAR_ALL: u16 = 0xFFFF;
pub(crate) const NORMAL_INT_REQUEST_MASK: u16 = NORMAL_INT_CMD_COMPLETE
    | NORMAL_INT_XFER_COMPLETE
    | NORMAL_INT_BLOCK_GAP
    | NORMAL_INT_DMA_INTERRUPT
    | NORMAL_INT_BUFFER_WRITE_READY
    | NORMAL_INT_BUFFER_READ_READY
    | NORMAL_INT_ERROR;

// ── Error Interrupt Status ─────────────────────────────────────────────

pub(crate) const ERROR_INT_CMD_TIMEOUT: u16 = 1 << 0;
pub(crate) const ERROR_INT_CMD_CRC: u16 = 1 << 1;
pub(crate) const ERROR_INT_CMD_END_BIT: u16 = 1 << 2;
pub(crate) const ERROR_INT_CMD_INDEX: u16 = 1 << 3;
pub(crate) const ERROR_INT_DATA_TIMEOUT: u16 = 1 << 4;
pub(crate) const ERROR_INT_DATA_CRC: u16 = 1 << 5;
pub(crate) const ERROR_INT_DATA_END_BIT: u16 = 1 << 6;
pub(crate) const ERROR_INT_CURRENT_LIMIT: u16 = 1 << 7;
pub(crate) const ERROR_INT_AUTO_CMD: u16 = 1 << 8;
pub(crate) const ERROR_INT_ADMA: u16 = 1 << 9;
pub(crate) const ERROR_INT_CLEAR_ALL: u16 = 0xFFFF;

pub(crate) const ERROR_INT_CMD_LINE_MASK: u16 =
    ERROR_INT_CMD_TIMEOUT | ERROR_INT_CMD_CRC | ERROR_INT_CMD_END_BIT | ERROR_INT_CMD_INDEX;

pub(crate) const ERROR_INT_DATA_LINE_MASK: u16 =
    ERROR_INT_DATA_TIMEOUT | ERROR_INT_DATA_CRC | ERROR_INT_DATA_END_BIT;

pub(crate) const ERROR_INT_DATA_OR_ADMA_MASK: u16 = ERROR_INT_DATA_LINE_MASK | ERROR_INT_ADMA;

// ── Host Control 1 ─────────────────────────────────────────────────────

pub(crate) const HOST_CTRL1_4BIT: u8 = 1 << 1;
pub(crate) const HOST_CTRL1_HIGH_SPEED: u8 = 1 << 2;
pub(crate) const HOST_CTRL1_8BIT: u8 = 1 << 5;

// DMA select (HOST_CONTROL1 bits 4..3):
//   00 = SDMA, 10 = 32-bit ADMA2, 11 = 64-bit ADMA2 (v4)
pub(crate) const HOST_CTRL1_DMA_SEL_MASK: u8 = 0b11 << 3;
pub(crate) const HOST_CTRL1_DMA_SEL_SDMA: u8 = 0b00 << 3;
pub(crate) const HOST_CTRL1_DMA_SEL_ADMA2_32: u8 = 0b10 << 3;
pub(crate) const HOST_CTRL1_DMA_SEL_ADMA2_64: u8 = 0b11 << 3;

// ── Capabilities ───────────────────────────────────────────────────────

pub(crate) const CAPS_LOW_ADMA2_SUPPORTED: u32 = 1 << 19;
pub(crate) const CAPS_LOW_64BIT_SYSBUS_V3: u32 = 1 << 28;

// ── Power Control ──────────────────────────────────────────────────────

pub(crate) const POWER_ON: u8 = 1 << 0;
pub(crate) const POWER_180: u8 = 0x0A;
pub(crate) const POWER_300: u8 = 0x0C;
pub(crate) const POWER_330: u8 = 0x0E;

// ── Clock Control ──────────────────────────────────────────────────────

pub(crate) const CLOCK_INTERNAL_ENABLE: u16 = 1 << 0;
pub(crate) const CLOCK_INTERNAL_STABLE: u16 = 1 << 1;
pub(crate) const CLOCK_SD_ENABLE: u16 = 1 << 2;

// ── Host Control 2 (UHS-I, tuning, 1.8 V) ─────────────────────────────

/// UHS_MODE_SELECT bits 2..0: 0 = SDR12, 1 = SDR25, 2 = SDR50,
/// 3 = SDR104 / HS200, 4 = DDR50, 5 = HS400.
pub(crate) const HOST_CTRL2_UHS_MODE_MASK: u16 = 0b111;
pub(crate) const HOST_CTRL2_UHS_SDR12: u16 = 0;
pub(crate) const HOST_CTRL2_UHS_SDR25: u16 = 1;
pub(crate) const HOST_CTRL2_UHS_SDR50: u16 = 2;
pub(crate) const HOST_CTRL2_UHS_SDR104: u16 = 3;
pub(crate) const HOST_CTRL2_UHS_DDR50: u16 = 4;
pub(crate) const HOST_CTRL2_UHS_HS400: u16 = 5;

/// 1.8 V signaling enable. 0 = 3.3 V, 1 = 1.8 V.
pub(crate) const HOST_CTRL2_1V8_SIGNALING: u16 = 1 << 3;
/// Driver strength type select (bits 4-5). 0 = type B (default).
pub(crate) const HOST_CTRL2_DRIVER_STRENGTH_MASK: u16 = 0b11 << 4;
/// Execute Tuning — set by software, controller clears it when the
/// loop is done.
pub(crate) const HOST_CTRL2_EXECUTE_TUNING: u16 = 1 << 6;
/// Sampling Clock Select — controller-set after tuning. 1 = tuning
/// produced a stable phase, 0 = no stable phase / tuning failed.
pub(crate) const HOST_CTRL2_SAMPLING_CLOCK_SELECT: u16 = 1 << 7;

// ── Transfer Mode ──────────────────────────────────────────────────────

pub(crate) const XFER_MODE_DMA_ENABLE: u16 = 1 << 0;
pub(crate) const XFER_MODE_BLOCK_COUNT_ENABLE: u16 = 1 << 1;
pub(crate) const XFER_MODE_AUTO_CMD12: u16 = 1 << 2;
pub(crate) const XFER_MODE_READ: u16 = 1 << 4;
pub(crate) const XFER_MODE_MULTI_BLOCK: u16 = 1 << 5;

// ── Command register encoding ──────────────────────────────────────────

pub(crate) const CMD_RESP_NONE: u16 = 0b00;
pub(crate) const CMD_RESP_LEN136: u16 = 0b01;
pub(crate) const CMD_RESP_LEN48: u16 = 0b10;
pub(crate) const CMD_RESP_LEN48_BUSY: u16 = 0b11;
pub(crate) const CMD_CRC_CHECK: u16 = 1 << 3;
pub(crate) const CMD_INDEX_CHECK: u16 = 1 << 4;
pub(crate) const CMD_DATA_PRESENT: u16 = 1 << 5;
