//! SDHCI 标准寄存器定义 (SD Host Controller Spec v3.0)
//!
//! 仅包含 SDHCI 控制器寄存器，不包含任何 WiFi 芯片特定常量。

/// SDHCI 标准寄存器偏移量
pub const SDHCI_DMA_ADDRESS: u32 = 0x00;
pub const SDHCI_BLOCK_SIZE: u32 = 0x04;
pub const SDHCI_BLOCK_COUNT: u32 = 0x06;
pub const SDHCI_ARGUMENT: u32 = 0x08;
pub const SDHCI_TRANSFER_MODE: u32 = 0x0C;
pub const SDHCI_COMMAND: u32 = 0x0E;
pub const SDHCI_RESPONSE: u32 = 0x10;
pub const SDHCI_BUFFER: u32 = 0x20;
pub const SDHCI_PRESENT_STATE: u32 = 0x24;
pub const SDHCI_HOST_CONTROL: u32 = 0x28;
pub const SDHCI_POWER_CONTROL: u32 = 0x29;
pub const SDHCI_CLOCK_CONTROL: u32 = 0x2C;
pub const SDHCI_TIMEOUT_CONTROL: u32 = 0x2E;
pub const SDHCI_SOFTWARE_RESET: u32 = 0x2F;

/// ---- 中断寄存器 (16-bit 分离访问) ----
pub const SDHCI_INT_STATUS_NORM: u32 = 0x30;
pub const SDHCI_INT_STATUS_ERR: u32 = 0x32;
pub const SDHCI_NORM_INT_STS_EN: u32 = 0x34;
pub const SDHCI_ERR_INT_STS_EN: u32 = 0x36;
pub const SDHCI_NORM_INT_SIG_EN: u32 = 0x38;
pub const SDHCI_ERR_INT_SIG_EN: u32 = 0x3A;

pub const SDHCI_CAPABILITIES: u32 = 0x40;
pub const SDHCI_HOST_VERSION: u32 = 0xFE;

/// Present State Register (0x24) 位定义
pub const SDHCI_CMD_INHIBIT: u32 = 1 << 0;
pub const SDHCI_DATA_INHIBIT: u32 = 1 << 1;
pub const SDHCI_BUF_WR_EN: u32 = 1 << 10;
pub const SDHCI_BUF_RD_EN: u32 = 1 << 11;
pub const SDHCI_CARD_INSERTED: u32 = 1 << 16;

/// Normal Interrupt Status (0x30) 位定义 (16-bit)
pub const NORM_INT_CMD_COMPLETE: u16 = 1 << 0;
pub const NORM_INT_XFER_COMPLETE: u16 = 1 << 1;
pub const NORM_INT_BUF_WR_READY: u16 = 1 << 4;
pub const NORM_INT_BUF_RD_READY: u16 = 1 << 5;
pub const NORM_INT_CARD_INT: u16 = 1 << 8;
pub const NORM_INT_ERROR: u16 = 1 << 15;

/// Error Interrupt Status (0x32) 位定义 (16-bit)
pub const ERR_INT_CMD_TIMEOUT: u16 = 1 << 0;
pub const ERR_INT_CMD_CRC: u16 = 1 << 1;
pub const ERR_INT_CMD_END_BIT: u16 = 1 << 2;
pub const ERR_INT_CMD_INDEX: u16 = 1 << 3;
pub const ERR_INT_DAT_TIMEOUT: u16 = 1 << 4;
pub const ERR_INT_DAT_CRC: u16 = 1 << 5;
pub const ERR_INT_DAT_END_BIT: u16 = 1 << 6;

/// 组合掩码
/// Status Enable: 使能所有需要的状态位
pub const NORM_INT_ENABLE_MASK: u16 = NORM_INT_CMD_COMPLETE
    | NORM_INT_XFER_COMPLETE
    | NORM_INT_BUF_WR_READY
    | NORM_INT_BUF_RD_READY
    | NORM_INT_CARD_INT;

pub const ERR_INT_ENABLE_MASK: u16 = ERR_INT_CMD_TIMEOUT
    | ERR_INT_CMD_CRC
    | ERR_INT_CMD_END_BIT
    | ERR_INT_CMD_INDEX
    | ERR_INT_DAT_TIMEOUT
    | ERR_INT_DAT_CRC
    | ERR_INT_DAT_END_BIT;

pub const ERR_INT_CMD_MASK: u16 =
    ERR_INT_CMD_TIMEOUT | ERR_INT_CMD_CRC | ERR_INT_CMD_END_BIT | ERR_INT_CMD_INDEX;

pub const ERR_INT_DAT_MASK: u16 = ERR_INT_DAT_TIMEOUT | ERR_INT_DAT_CRC | ERR_INT_DAT_END_BIT;

/// Signal Enable: 仅使能 CARD_INT (PIO 事件由 wait 函数直接轮询 INT_STATUS)
pub const NORM_INT_SIG_MASK: u16 = NORM_INT_CARD_INT;

pub const ERR_INT_SIG_MASK: u16 = 0;

/// Transfer Mode Register (0x0C) 位定义 (16-bit)
pub const TM_BLK_CNT_EN: u16 = 1 << 1;
pub const TM_DATA_DIR_READ: u16 = 1 << 4;
pub const TM_MULTI_BLOCK: u16 = 1 << 5;

/// BLOCK_SIZE register SDMA buffer boundary (bits [14:12])
/// 0x7 = 512 KiB boundary, standard default even in PIO mode
pub const SDHCI_SDMA_BOUNDARY_512K: u16 = 0x7 << 12;

/// Clock Control Register (0x2C) 位定义
pub const CC_INT_CLK_EN: u16 = 0x0001;
pub const CC_INT_CLK_STABLE: u16 = 0x0002;
pub const CC_SD_CLK_EN: u16 = 0x0004;
pub const CC_FREQ_SEL_EXT_MASK: u16 = 0x00C0;
pub const CC_FREQ_SEL_MASK: u16 = 0xFF00;
pub const CC_DIV_SHIFT: u32 = 8;
pub const CC_EXT_DIV_SHIFT: u32 = 6;

/// Software Reset Register (0x2F) 位定义
pub const SWRST_ALL: u8 = 0x01;
pub const SWRST_CMD_LINE: u8 = 0x02;
pub const SWRST_DAT_LINE: u8 = 0x04;

/// Power Control Register (0x29) 位定义
pub const POWER_ON: u8 = 0x01;
pub const POWER_VSEL_33V: u8 = 0x07 << 1;
pub const POWER_330V_ON: u8 = POWER_ON | POWER_VSEL_33V;

/// Host Control 1 (0x28) 位定义
pub const HC_BUS_WIDTH_4: u8 = 0x02; // bit 1: 4-bit mode
pub const HC_HIGH_SPEED: u8 = 0x04; // bit 2: High Speed Enable
pub const HC_CARD_DET_TEST: u8 = 0x40; // bit 6: Card Detect Test Level
pub const HC_CARD_DET_SEL: u8 = 0x80; // bit 7: Card Detect Signal Selection

/// Command Register (0x0E) 位定义 (16-bit)
/// 命令索引左移位数（bits[13:8]）
pub const CMD_INDEX_SHIFT: u32 = 8;

// ---- Response Type Select (bits[1:0]) ----
pub const CMD_RESP_NONE: u16 = 0x00; // 00: 无响应
pub const CMD_RESP_136: u16 = 0x01; // 01: 136-bit 响应 (R2)
pub const CMD_RESP_48: u16 = 0x02; // 10: 48-bit 响应
pub const CMD_RESP_48_BUSY: u16 = 0x03; // 11: 48-bit 响应 + busy

// ---- 控制位 ----
pub const CMD_CRC_CHECK_EN: u16 = 1 << 3; // bit 3: CRC 校验使能
pub const CMD_INDEX_CHECK_EN: u16 = 1 << 4; // bit 4: 索引校验使能
pub const CMD_DATA_PRESENT: u16 = 1 << 5; // bit 5: 有数据传输

// ---- 组合标志（按 SD/SDIO 响应类型）----
/// R4: 48-bit, 无 CRC/索引校验 (CMD5)
pub const CMD_FLAGS_R4: u16 = CMD_RESP_48;
/// R5/R6: 48-bit + CRC + 索引校验 (CMD3, CMD52)
pub const CMD_FLAGS_R5: u16 = CMD_RESP_48 | CMD_CRC_CHECK_EN | CMD_INDEX_CHECK_EN;
/// R1b: 48-bit busy + CRC + 索引校验 (CMD7)
pub const CMD_FLAGS_R1B: u16 = CMD_RESP_48_BUSY | CMD_CRC_CHECK_EN | CMD_INDEX_CHECK_EN;
/// R5 + 数据: 48-bit + CRC + 索引 + data present (CMD53)
pub const CMD_FLAGS_R5_DATA: u16 = CMD_FLAGS_R5 | CMD_DATA_PRESENT;

/// 时钟频率常量 (Hz)
pub const INIT_CLOCK_HZ: u32 = 400_000; // SD 规范初始化时钟 ≤ 400KHz
// LicheeRV Nano WiFi SDIO DTS caps max-frequency at 25 MHz. The AIC8800
// enumerates at 50 MHz but large CMD53 firmware writes are not reliable there.
pub const HIGH_SPEED_CLOCK_HZ: u32 = 25_000_000;
pub const DEFAULT_CLOCK_HZ: u32 = 25_000_000; // 默认时钟 25MHz
// LicheeRV Nano vendor DTS uses src-frequency = 375 MHz for wifi-sd@4320000.
// The CVI SDIO controller capabilities register is not a reliable source here.
pub const CVI_SDIO_SRC_CLOCK_HZ: u32 = 375_000_000;

/// 时钟分频器常量
pub const MHZ_TO_HZ: u32 = 1_000_000; // MHz 到 Hz 的转换因子
pub const DIV_FACTOR: u32 = 2; // SDHCI 分频因子（固定为 2）
pub const MAX_DIVISOR: u16 = 0x3FF; // 10-bit 分频器最大值 (1023)
pub const DIVISOR_LOW_MASK: u16 = 0xFF; // 低 8 位掩码
pub const DIVISOR_HIGH_MASK: u16 = 0x03; // 高 2 位掩码

/// ---- Vendor registers (CV1800/SG2002 specific, offset from controller base) ----
pub const VENDOR_MSHC_CTRL: u32 = 0x200;
pub const VENDOR_PHY_TX_RX_DLY: u32 = 0x240;
pub const VENDOR_PHY_CONFIG: u32 = 0x24C;

/// VENDOR_MSHC_CTRL (0x200) bit definitions
pub const VENDOR_MSHC_CTRL_FEEDBACK_CLK: u32 = 1 << 1;
pub const VENDOR_MSHC_CTRL_TX_DLY_EN: u32 = 1 << 8;
pub const VENDOR_MSHC_CTRL_RX_DLY_EN: u32 = 1 << 9;
pub const VENDOR_MSHC_CTRL_SD1_SEL: u32 = 1 << 16;

/// VENDOR_PHY_CONFIG (0x24C) bit definitions
pub const VENDOR_PHY_ENABLE: u32 = 1 << 0;

/// Capabilities Register 时钟基频字段
pub const CAPS_BASE_FREQ_SHIFT: u32 = 8; // 基频字段起始位
pub const CAPS_BASE_FREQ_MASK: u32 = 0xFF; // 基频字段掩码 (8 bits)

/// 超时常量 (循环次数)
pub const RESET_TIMEOUT: u32 = 100_000;
pub const CLOCK_STABLE_TIMEOUT: u32 = 100_000;
pub const CMD_RESPONSE_TIMEOUT: u32 = 100_000;
pub const CMD5_READY_TIMEOUT: u32 = 1_000;
pub const CMD5_OCR_RETRY: u32 = 1000;
pub const PIO_TIMEOUT: u32 = 1_000_000;
pub const FUNC_READY_TIMEOUT: u32 = 1_000;
pub const FUNC_READY_DELAY: u32 = 100_000;
