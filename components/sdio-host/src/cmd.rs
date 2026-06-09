//! SDIO 命令和响应常量  
//!  
//! 包含 SD 命令号、OCR 位定义、R5 响应标志、CMD52/CMD53 参数构造常量。  
//! 不包含 SDHCI 控制器寄存器位（Transfer Mode、Present State 等属于 SDHCI 层）。  

/// SD 命令号
pub const CMD0_GO_IDLE: u8 = 0;
pub const CMD3_SEND_REL_ADDR: u8 = 3;
pub const CMD5_IO_SEND_OP_COND: u8 = 5;
pub const CMD7_SELECT_CARD: u8 = 7;
pub const CMD52_IO_RW_DIRECT: u8 = 52;
pub const CMD53_IO_RW_EXTENDED: u8 = 53;

// CMD5 OCR (IO Operation Conditions Register) 位定义
/// bit 31: Card Ready — 卡完成上电初始化后置 1  
pub const OCR_IORDY: u32 = 1 << 31;

/// bits 30:28: Number of I/O Functions (1-7)  
pub const OCR_IO_FUNC_SHIFT: u32 = 28;
pub const OCR_IO_FUNC_MASK: u32 = 0x7 << OCR_IO_FUNC_SHIFT;

/// bit 27: Memory Present — 卡同时具有存储功能 (combo card)  
pub const OCR_MEM_PRESENT: u32 = 1 << 27;

/// bits 23:0: 电压窗口 (Voltage Window)  
pub const OCR_VOLTAGE_MASK: u32 = 0x00FF_8000;

/// 3.2-3.3V | 3.3-3.4V (bit 20 + bit 21)  
pub const OCR_3V2_3V4: u32 = 0x0030_0000;

/// R5 响应标志位 (CMD52/CMD53 response)
pub const R5_COM_CRC_ERROR: u32 = 1 << 15;
pub const R5_ILLEGAL_COMMAND: u32 = 1 << 14;
pub const R5_IO_CURRENT_STATE: u32 = 0x3 << 12;
pub const R5_ERROR: u32 = 1 << 11;
pub const R5_FUNCTION_NUMBER: u32 = 1 << 9;
pub const R5_OUT_OF_RANGE: u32 = 1 << 8;
/// R5 响应数据字节掩码 (bits 7:0)  
pub const R5_DATA_MASK: u32 = 0xFF;

pub const R5_ERROR_MASK: u32 =
    R5_COM_CRC_ERROR | R5_ILLEGAL_COMMAND | R5_ERROR | R5_FUNCTION_NUMBER | R5_OUT_OF_RANGE;

/// R4 响应 bit 31: Card Ready（与 OCR_IORDY 相同，语义别名）  
pub const R4_READY: u32 = OCR_IORDY;

/// R4 响应中 OCR 电压窗口掩码（bits 8-23）  
pub const R4_OCR_MASK: u32 = 0x00FF_FF00;

/// CMD52/CMD53 参数构造常量
pub const CMD52_RW_FLAG: u32 = 1 << 31;
pub const CMD52_RAW_FLAG: u32 = 1 << 27; // Read After Write  
pub const CMD53_RW_FLAG: u32 = 1 << 31;
pub const CMD53_BLOCK_MODE: u32 = 1 << 27;
pub const CMD53_OP_CODE_INC: u32 = 1 << 26; // incrementing address  

/// 17-bit SDIO 地址掩码  
pub const SDIO_ADDR_MASK: u32 = 0x1FFFF;
