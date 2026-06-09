//! SDIO CCCR (Card Common Control Registers) 和 FBR 常量  
//!  
//! 参考: SD Specifications Part E1 — SDIO Simplified Specification  

/// CCCR 寄存器地址 (Function 0 地址空间)
pub const CCCR_SDIO_REVISION: u32 = 0x00;
pub const CCCR_SD_REVISION: u32 = 0x01;
pub const CCCR_IO_ENABLE: u32 = 0x02;
pub const CCCR_IO_READY: u32 = 0x03;
pub const CCCR_INT_ENABLE: u32 = 0x04;
pub const CCCR_INT_PENDING: u32 = 0x05;
pub const CCCR_IO_ABORT: u32 = 0x06;
pub const CCCR_BUS_INTERFACE: u32 = 0x07;
pub const CCCR_CARD_CAPABILITY: u32 = 0x08;
pub const CCCR_CIS_POINTER: u32 = 0x09; // 3 bytes (0x09-0x0B)  
pub const CCCR_BUS_SUSPEND: u32 = 0x0C;
pub const CCCR_FUNCTION_SELECT: u32 = 0x0D;
pub const CCCR_EXEC_FLAGS: u32 = 0x0E;
pub const CCCR_READY_FLAGS: u32 = 0x0F;
pub const CCCR_FN0_BLOCK_SIZE: u32 = 0x10; // 2 bytes (0x10-0x11)  
pub const CCCR_POWER_CONTROL: u32 = 0x12;

/// Bus Speed Select (CCCR v3.0+, SDIO 3.0)  
///  
/// bit\[0\]: SHS — Support High-Speed (read-only)  
/// bit\[1\]: EHS — Enable High-Speed (read/write)  
/// bit\[3:2\]: BSS — Bus Speed Select for UHS  
pub const CCCR_BUS_SPEED_SELECT: u32 = 0x13;

/// FBR (Function Basic Registers)
/// Function N 的 FBR 基地址 = 0x100 * N
/// FBR 基地址计算  
pub const fn fbr_base(func: u8) -> u32 {
    (func as u32) * 0x100
}

/// Block size 寄存器偏移（2 bytes, 相对于 FBR 基地址）  
pub const FBR_BLOCK_SIZE_OFFSET: u32 = 0x10;

/// CIS Pointer 偏移（3 bytes, 相对于 FBR 基地址）  
pub const FBR_CIS_PTR_OFFSET: u32 = 0x09;

/// Bus width 设置值 (CCCR_BUS_INTERFACE bits[1:0])
pub const BUS_WIDTH_1BIT: u8 = 0x00;
pub const BUS_WIDTH_4BIT: u8 = 0x02;
pub const BUS_WIDTH_MASK: u8 = 0x03;

/// CIS Tuple codes
pub const CISTPL_NULL: u8 = 0x00;
pub const CISTPL_MANFID: u8 = 0x20;
pub const CISTPL_FUNCID: u8 = 0x21;
pub const CISTPL_FUNCE: u8 = 0x22;
pub const CISTPL_END: u8 = 0xFF;

/// SDIO 标准块大小 (bytes)  
pub const SDIO_DEFAULT_BLOCK_SIZE: u16 = 512;
