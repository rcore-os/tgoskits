//! SDIO 主机控制器抽象层

#![no_std]

extern crate alloc;

pub mod cccr;
pub mod cmd;
pub mod error;

use alloc::sync::Arc;

use error::SdioError;

/// SDIO Card Interrupt 控制（无锁，ISR 安全）
///
/// mask/unmask 是单次 MMIO 写操作，不涉及控制器状态机，
/// 因此不需要与 CMD52/CMD53 互斥。
pub trait SdioCardIrq: Send + Sync {
    fn mask_card_irq(&self);
    fn unmask_card_irq(&self);

    /// Consumes one card-interrupt fact captured by the registered endpoint.
    ///
    /// Implementations return `false` when no acknowledged event is pending;
    /// this method must never read or clear a destructive controller register.
    fn take_card_irq(&self) -> bool {
        false
    }
}

/// SDIO 主机控制器抽象
///
/// 实现者负责：
/// - SDHCI 控制器初始化和 SDIO 卡枚举（CMD5 → CMD3 → CMD7）
/// - CMD52 单字节读写（I/O read/write direct）
/// - CMD53 多字节/块读写（I/O read/write extended）
/// - Function 使能和 block size 设置
pub trait SdioHost: Send + Sync {
    /// 初始化 SDHCI 控制器，执行 SDIO 卡枚举
    fn init(&mut self) -> Result<(), SdioError>;

    /// 获取控制器 MMIO 基地址（ISR 裸写用）
    fn mmio_base(&self) -> usize;

    /// CMD52: 单字节读 (I/O read direct)
    fn read_byte(&self, func: u8, addr: u32) -> Result<u8, SdioError>;

    /// CMD52: 单字节写 (I/O write direct)
    fn write_byte(&self, func: u8, addr: u32, val: u8) -> Result<(), SdioError>;

    /// CMD52: 写后读 (I/O write direct, RAW 模式)
    fn write_byte_read(&self, func: u8, addr: u32, val: u8) -> Result<u8, SdioError>;

    /// CMD53: 多字节/块读 (I/O read extended, fixed address / FIFO 模式)
    fn read_fifo(&self, func: u8, addr: u32, buf: &mut [u8]) -> Result<(), SdioError>;

    /// CMD53: 多字节/块读 (I/O read extended, incrementing address 模式)
    fn read_fifo_inc(&self, func: u8, addr: u32, buf: &mut [u8]) -> Result<(), SdioError>;

    /// CMD53: 多字节/块写 (I/O write extended, fixed address / FIFO 模式)
    fn write_fifo(&self, func: u8, addr: u32, buf: &[u8]) -> Result<(), SdioError>;

    /// CMD53: 多字节/块写 (I/O write extended, incrementing address 模式)
    fn write_fifo_inc(&self, func: u8, addr: u32, buf: &[u8]) -> Result<(), SdioError>;

    /// 设置指定 function 的 block size
    fn set_block_size(&self, func: u8, size: u16) -> Result<(), SdioError>;

    /// 设置 SDIO 时钟频率（Hz）
    fn set_clock(&self, _hz: u32) -> Result<(), SdioError>;

    /// 使能指定 SDIO function
    fn enable_func(&self, func: u8) -> Result<(), SdioError>;

    /// 获取 SDIO 卡的 vendor/device ID
    fn vendor_device_id(&self) -> (u16, u16);

    /// 使能 SDHCI 中断信号（切换到中断驱动模式）
    fn enable_irq(&self);

    /// 禁用 SDHCI 中断信号
    fn disable_irq(&self);

    /// 创建无锁的 Card IRQ 控制句柄
    fn card_irq_ctrl(&self) -> Option<Arc<dyn SdioCardIrq>>;
}
