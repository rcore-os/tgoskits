//! SD Host Controller register offsets and bit definitions.
//!
//! Layout matches the SD Host Controller Standard Specification (v3.00 /
//! v4.00). Only the fields the MVP driver actually touches are spelled out;
//! the rest of the register file is reachable via the raw offset constants.

#![allow(dead_code)]

use tock_registers::register_bitfields;

register_bitfields! [
    u8,
    pub SOFTWARE_RESET [
        ALL OFFSET(0) NUMBITS(1) [],
        COMMAND OFFSET(1) NUMBITS(1) [],
        DATA OFFSET(2) NUMBITS(1) []
    ],
    pub HOST_CONTROL1 [
        DATA_WIDTH_4 OFFSET(1) NUMBITS(1) [],
        HIGH_SPEED OFFSET(2) NUMBITS(1) [],
        DMA_SELECT OFFSET(3) NUMBITS(2) [Sdma = 0, Adma2_32 = 2, Adma2_64 = 3],
        DATA_WIDTH_8 OFFSET(5) NUMBITS(1) []
    ],
    pub POWER_CONTROL [
        BUS_POWER OFFSET(0) NUMBITS(1) [],
        VOLTAGE OFFSET(1) NUMBITS(3) [V180 = 5, V300 = 6, V330 = 7]
    ]
];

register_bitfields! [
    u16,
    pub NORMAL_INTERRUPT [
        COMMAND_COMPLETE OFFSET(0) NUMBITS(1) [],
        TRANSFER_COMPLETE OFFSET(1) NUMBITS(1) [],
        BLOCK_GAP OFFSET(2) NUMBITS(1) [],
        DMA OFFSET(3) NUMBITS(1) [],
        BUFFER_WRITE_READY OFFSET(4) NUMBITS(1) [],
        BUFFER_READ_READY OFFSET(5) NUMBITS(1) [],
        CARD_INSERTION OFFSET(6) NUMBITS(1) [],
        CARD_REMOVAL OFFSET(7) NUMBITS(1) [],
        CARD_INTERRUPT OFFSET(8) NUMBITS(1) [],
        ERROR OFFSET(15) NUMBITS(1) []
    ],
    pub ERROR_INTERRUPT [
        COMMAND_TIMEOUT OFFSET(0) NUMBITS(1) [],
        COMMAND_CRC OFFSET(1) NUMBITS(1) [],
        COMMAND_END_BIT OFFSET(2) NUMBITS(1) [],
        COMMAND_INDEX OFFSET(3) NUMBITS(1) [],
        DATA_TIMEOUT OFFSET(4) NUMBITS(1) [],
        DATA_CRC OFFSET(5) NUMBITS(1) [],
        DATA_END_BIT OFFSET(6) NUMBITS(1) [],
        CURRENT_LIMIT OFFSET(7) NUMBITS(1) [],
        AUTO_COMMAND OFFSET(8) NUMBITS(1) [],
        ADMA OFFSET(9) NUMBITS(1) []
    ],
    pub CLOCK_CONTROL [
        INTERNAL_ENABLE OFFSET(0) NUMBITS(1) [],
        INTERNAL_STABLE OFFSET(1) NUMBITS(1) [],
        SD_ENABLE OFFSET(2) NUMBITS(1) []
    ],
    pub HOST_CONTROL2 [
        UHS_MODE OFFSET(0) NUMBITS(3) [Sdr12 = 0, Sdr25 = 1, Sdr50 = 2, Sdr104 = 3, Ddr50 = 4, Hs400 = 5],
        SIGNALING_1V8 OFFSET(3) NUMBITS(1) [],
        DRIVER_STRENGTH OFFSET(4) NUMBITS(2) [],
        EXECUTE_TUNING OFFSET(6) NUMBITS(1) [],
        SAMPLING_CLOCK OFFSET(7) NUMBITS(1) [],
        V4_MODE OFFSET(12) NUMBITS(1) [],
        ADDRESSING_64BIT OFFSET(13) NUMBITS(1) []
    ],
    pub TRANSFER_MODE [
        DMA_ENABLE OFFSET(0) NUMBITS(1) [],
        BLOCK_COUNT_ENABLE OFFSET(1) NUMBITS(1) [],
        AUTO_CMD12 OFFSET(2) NUMBITS(1) [],
        READ OFFSET(4) NUMBITS(1) [],
        MULTI_BLOCK OFFSET(5) NUMBITS(1) []
    ],
    pub COMMAND [
        RESPONSE OFFSET(0) NUMBITS(2) [None = 0, Length136 = 1, Length48 = 2, Length48Busy = 3],
        CRC_CHECK OFFSET(3) NUMBITS(1) [],
        INDEX_CHECK OFFSET(4) NUMBITS(1) [],
        DATA_PRESENT OFFSET(5) NUMBITS(1) []
    ]
];

register_bitfields! [
    u32,
    pub PRESENT_STATE [
        COMMAND_INHIBIT OFFSET(0) NUMBITS(1) [],
        DATA_INHIBIT OFFSET(1) NUMBITS(1) [],
        BUFFER_WRITE_ENABLE OFFSET(10) NUMBITS(1) [],
        BUFFER_READ_ENABLE OFFSET(11) NUMBITS(1) [],
        CARD_INSERTED OFFSET(16) NUMBITS(1) [],
        DATA_LINES OFFSET(20) NUMBITS(4) []
    ],
    pub CAPABILITIES_LOW [
        ADMA2 OFFSET(19) NUMBITS(1) [],
        SYSBUS_64_V4 OFFSET(27) NUMBITS(1) [],
        SYSBUS_64_V3 OFFSET(28) NUMBITS(1) []
    ]
];

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

pub(crate) const PRESENT_CMD_INHIBIT: u32 = PRESENT_STATE::COMMAND_INHIBIT::SET.value;
pub(crate) const PRESENT_DAT_INHIBIT: u32 = PRESENT_STATE::DATA_INHIBIT::SET.value;
pub(crate) const PRESENT_BUFFER_WRITE_ENABLE: u32 = PRESENT_STATE::BUFFER_WRITE_ENABLE::SET.value;
pub(crate) const PRESENT_BUFFER_READ_ENABLE: u32 = PRESENT_STATE::BUFFER_READ_ENABLE::SET.value;
pub(crate) const PRESENT_CARD_INSERTED: u32 = PRESENT_STATE::CARD_INSERTED::SET.value;
pub(crate) const PRESENT_DAT0_LINE_SIGNAL_LEVEL: u32 = PRESENT_STATE::DATA_LINES.val(1).value;
pub(crate) const PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL: u32 = PRESENT_STATE::DATA_LINES.val(0xf).value;

// ── Software Reset ─────────────────────────────────────────────────────

pub(crate) const RESET_ALL: u8 = SOFTWARE_RESET::ALL::SET.value;
pub(crate) const RESET_CMD: u8 = SOFTWARE_RESET::COMMAND::SET.value;
pub(crate) const RESET_DAT: u8 = SOFTWARE_RESET::DATA::SET.value;

// ── Normal Interrupt Status ────────────────────────────────────────────

pub(crate) const NORMAL_INT_CMD_COMPLETE: u16 = NORMAL_INTERRUPT::COMMAND_COMPLETE::SET.value;
pub(crate) const NORMAL_INT_XFER_COMPLETE: u16 = NORMAL_INTERRUPT::TRANSFER_COMPLETE::SET.value;
pub(crate) const NORMAL_INT_BLOCK_GAP: u16 = NORMAL_INTERRUPT::BLOCK_GAP::SET.value;
pub(crate) const NORMAL_INT_DMA_INTERRUPT: u16 = NORMAL_INTERRUPT::DMA::SET.value;
pub(crate) const NORMAL_INT_BUFFER_WRITE_READY: u16 =
    NORMAL_INTERRUPT::BUFFER_WRITE_READY::SET.value;
pub(crate) const NORMAL_INT_BUFFER_READ_READY: u16 = NORMAL_INTERRUPT::BUFFER_READ_READY::SET.value;
pub(crate) const NORMAL_INT_CARD_INSERTION: u16 = NORMAL_INTERRUPT::CARD_INSERTION::SET.value;
pub(crate) const NORMAL_INT_CARD_REMOVAL: u16 = NORMAL_INTERRUPT::CARD_REMOVAL::SET.value;
pub(crate) const NORMAL_INT_CARD_INTERRUPT: u16 = NORMAL_INTERRUPT::CARD_INTERRUPT::SET.value;
pub(crate) const NORMAL_INT_ERROR: u16 = NORMAL_INTERRUPT::ERROR::SET.value;
pub(crate) const NORMAL_INT_CLEAR_ALL: u16 = 0xFFFF;

// ── Error Interrupt Status ─────────────────────────────────────────────

pub(crate) const ERROR_INT_CMD_TIMEOUT: u16 = ERROR_INTERRUPT::COMMAND_TIMEOUT::SET.value;
pub(crate) const ERROR_INT_CMD_CRC: u16 = ERROR_INTERRUPT::COMMAND_CRC::SET.value;
pub(crate) const ERROR_INT_CMD_END_BIT: u16 = ERROR_INTERRUPT::COMMAND_END_BIT::SET.value;
pub(crate) const ERROR_INT_CMD_INDEX: u16 = ERROR_INTERRUPT::COMMAND_INDEX::SET.value;
pub(crate) const ERROR_INT_DATA_TIMEOUT: u16 = ERROR_INTERRUPT::DATA_TIMEOUT::SET.value;
pub(crate) const ERROR_INT_DATA_CRC: u16 = ERROR_INTERRUPT::DATA_CRC::SET.value;
pub(crate) const ERROR_INT_DATA_END_BIT: u16 = ERROR_INTERRUPT::DATA_END_BIT::SET.value;
pub(crate) const ERROR_INT_CURRENT_LIMIT: u16 = ERROR_INTERRUPT::CURRENT_LIMIT::SET.value;
pub(crate) const ERROR_INT_AUTO_CMD: u16 = ERROR_INTERRUPT::AUTO_COMMAND::SET.value;
pub(crate) const ERROR_INT_ADMA: u16 = ERROR_INTERRUPT::ADMA::SET.value;
pub(crate) const ERROR_INT_CLEAR_ALL: u16 = 0xFFFF;

pub(crate) const ERROR_INT_CMD_LINE_MASK: u16 =
    ERROR_INT_CMD_TIMEOUT | ERROR_INT_CMD_CRC | ERROR_INT_CMD_END_BIT | ERROR_INT_CMD_INDEX;

pub(crate) const ERROR_INT_DATA_LINE_MASK: u16 =
    ERROR_INT_DATA_TIMEOUT | ERROR_INT_DATA_CRC | ERROR_INT_DATA_END_BIT;

pub(crate) const ERROR_INT_DATA_OR_ADMA_MASK: u16 = ERROR_INT_DATA_LINE_MASK | ERROR_INT_ADMA;

// ── Host Control 1 ─────────────────────────────────────────────────────

pub(crate) const HOST_CTRL1_4BIT: u8 = HOST_CONTROL1::DATA_WIDTH_4::SET.value;
pub(crate) const HOST_CTRL1_HIGH_SPEED: u8 = HOST_CONTROL1::HIGH_SPEED::SET.value;
pub(crate) const HOST_CTRL1_8BIT: u8 = HOST_CONTROL1::DATA_WIDTH_8::SET.value;

// DMA select (HOST_CONTROL1 bits 4..3):
//   00 = SDMA, 10 = 32-bit ADMA2, 11 = 64-bit ADMA2 (v4)
pub(crate) const HOST_CTRL1_DMA_SEL_MASK: u8 = HOST_CONTROL1::DMA_SELECT.val(0b11).value;
pub(crate) const HOST_CTRL1_DMA_SEL_SDMA: u8 = HOST_CONTROL1::DMA_SELECT::Sdma.value;
pub(crate) const HOST_CTRL1_DMA_SEL_ADMA2_32: u8 = HOST_CONTROL1::DMA_SELECT::Adma2_32.value;
pub(crate) const HOST_CTRL1_DMA_SEL_ADMA2_64: u8 = HOST_CONTROL1::DMA_SELECT::Adma2_64.value;

// ── Capabilities ───────────────────────────────────────────────────────

pub(crate) const CAPS_LOW_ADMA2_SUPPORTED: u32 = CAPABILITIES_LOW::ADMA2::SET.value;
pub(crate) const CAPS_LOW_64BIT_SYSBUS_V4: u32 = CAPABILITIES_LOW::SYSBUS_64_V4::SET.value;
pub(crate) const CAPS_LOW_64BIT_SYSBUS_V3: u32 = CAPABILITIES_LOW::SYSBUS_64_V3::SET.value;

// ── Power Control ──────────────────────────────────────────────────────

pub(crate) const POWER_ON: u8 = POWER_CONTROL::BUS_POWER::SET.value;
pub(crate) const POWER_180: u8 = POWER_CONTROL::VOLTAGE::V180.value;
pub(crate) const POWER_300: u8 = POWER_CONTROL::VOLTAGE::V300.value;
pub(crate) const POWER_330: u8 = POWER_CONTROL::VOLTAGE::V330.value;

// ── Clock Control ──────────────────────────────────────────────────────

pub(crate) const CLOCK_INTERNAL_ENABLE: u16 = CLOCK_CONTROL::INTERNAL_ENABLE::SET.value;
pub(crate) const CLOCK_INTERNAL_STABLE: u16 = CLOCK_CONTROL::INTERNAL_STABLE::SET.value;
pub(crate) const CLOCK_SD_ENABLE: u16 = CLOCK_CONTROL::SD_ENABLE::SET.value;

// ── Host Control 2 (UHS-I, tuning, 1.8 V) ─────────────────────────────

/// UHS_MODE_SELECT bits 2..0: 0 = SDR12, 1 = SDR25, 2 = SDR50,
/// 3 = SDR104 / HS200, 4 = DDR50, 5 = HS400.
pub(crate) const HOST_CTRL2_UHS_MODE_MASK: u16 = HOST_CONTROL2::UHS_MODE.val(0b111).value;
pub(crate) const HOST_CTRL2_UHS_SDR12: u16 = HOST_CONTROL2::UHS_MODE::Sdr12.value;
pub(crate) const HOST_CTRL2_UHS_SDR25: u16 = HOST_CONTROL2::UHS_MODE::Sdr25.value;
pub(crate) const HOST_CTRL2_UHS_SDR50: u16 = HOST_CONTROL2::UHS_MODE::Sdr50.value;
pub(crate) const HOST_CTRL2_UHS_SDR104: u16 = HOST_CONTROL2::UHS_MODE::Sdr104.value;
pub(crate) const HOST_CTRL2_UHS_DDR50: u16 = HOST_CONTROL2::UHS_MODE::Ddr50.value;
pub(crate) const HOST_CTRL2_UHS_HS400: u16 = HOST_CONTROL2::UHS_MODE::Hs400.value;

/// 1.8 V signaling enable. 0 = 3.3 V, 1 = 1.8 V.
pub(crate) const HOST_CTRL2_1V8_SIGNALING: u16 = HOST_CONTROL2::SIGNALING_1V8::SET.value;
/// Driver strength type select (bits 4-5). 0 = type B (default).
pub(crate) const HOST_CTRL2_DRIVER_STRENGTH_MASK: u16 =
    HOST_CONTROL2::DRIVER_STRENGTH.val(0b11).value;
/// Execute Tuning — set by software, controller clears it when the
/// loop is done.
pub(crate) const HOST_CTRL2_EXECUTE_TUNING: u16 = HOST_CONTROL2::EXECUTE_TUNING::SET.value;
/// Sampling Clock Select — controller-set after tuning. 1 = tuning
/// produced a stable phase, 0 = no stable phase / tuning failed.
pub(crate) const HOST_CTRL2_SAMPLING_CLOCK_SELECT: u16 = HOST_CONTROL2::SAMPLING_CLOCK::SET.value;
pub(crate) const HOST_CTRL2_V4_MODE: u16 = HOST_CONTROL2::V4_MODE::SET.value;
pub(crate) const HOST_CTRL2_64BIT_ADDR: u16 = HOST_CONTROL2::ADDRESSING_64BIT::SET.value;

// ── Transfer Mode ──────────────────────────────────────────────────────

pub(crate) const XFER_MODE_DMA_ENABLE: u16 = TRANSFER_MODE::DMA_ENABLE::SET.value;
pub(crate) const XFER_MODE_BLOCK_COUNT_ENABLE: u16 = TRANSFER_MODE::BLOCK_COUNT_ENABLE::SET.value;
pub(crate) const XFER_MODE_AUTO_CMD12: u16 = TRANSFER_MODE::AUTO_CMD12::SET.value;
pub(crate) const XFER_MODE_READ: u16 = TRANSFER_MODE::READ::SET.value;
pub(crate) const XFER_MODE_MULTI_BLOCK: u16 = TRANSFER_MODE::MULTI_BLOCK::SET.value;

// ── Command register encoding ──────────────────────────────────────────

pub(crate) const CMD_RESP_NONE: u16 = COMMAND::RESPONSE::None.value;
pub(crate) const CMD_RESP_LEN136: u16 = COMMAND::RESPONSE::Length136.value;
pub(crate) const CMD_RESP_LEN48: u16 = COMMAND::RESPONSE::Length48.value;
pub(crate) const CMD_RESP_LEN48_BUSY: u16 = COMMAND::RESPONSE::Length48Busy.value;
pub(crate) const CMD_CRC_CHECK: u16 = COMMAND::CRC_CHECK::SET.value;
pub(crate) const CMD_INDEX_CHECK: u16 = COMMAND::INDEX_CHECK::SET.value;
pub(crate) const CMD_DATA_PRESENT: u16 = COMMAND::DATA_PRESENT::SET.value;
