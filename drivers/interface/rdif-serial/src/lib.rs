#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{
    any::Any,
    fmt::{Debug, Display},
    num::NonZeroU32,
};

use bitflags::bitflags;
pub use rdif_base::{DriverGeneric, KError};

pub type BIrqHandler = Box<dyn TIrqHandler>;
pub type BSender = Box<dyn TSender>;
pub type BReceiver = Box<dyn TReceiver>;
pub type BSerial = Box<dyn Interface>;

impl DriverGeneric for Box<dyn Interface> {
    fn name(&self) -> &str {
        self.as_ref().name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        self.as_ref().raw_any()
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        self.as_mut().raw_any_mut()
    }
}

mod serial;

pub use serial::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// 无效的波特率
    InvalidBaudrate,
    /// 不支持的数据位配置
    UnsupportedDataBits,
    /// 不支持的停止位配置
    UnsupportedStopBits,
    /// 不支持的奇偶校验配置
    UnsupportedParity,
    /// 寄存器访问错误
    RegisterError,
    /// 超时错误
    Timeout,
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransBytesError {
    pub bytes_transferred: usize,
    pub kind: TransferError,
}

impl Display for TransBytesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Transfer error after transferring {} bytes: {}",
            self.bytes_transferred, self.kind
        )
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    #[error("Data overrun by `{0:#x}`")]
    Overrun(u8),
    #[error("Parity error")]
    Parity,
    #[error("Framing error")]
    Framing,
    #[error("Break condition")]
    Break,
    #[error("Serial closed")]
    Closed,
}

/// 数据位配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataBits {
    Five  = 5,
    Six   = 6,
    Seven = 7,
    Eight = 8,
}

/// 停止位配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StopBits {
    One = 1,
    Two = 2,
}

/// 奇偶校验配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    None,
    Even,
    Odd,
    Mark,
    Space,
}

bitflags! {
    /// 中断状态标志
    #[derive(Debug, Clone, Copy)]
    pub struct InterruptMask: u32 {
        /// received data, including error data
        const RX_AVAILABLE = 0x01;
        const TX_EMPTY = 0x02;
    }
}

impl InterruptMask {
    pub fn rx_available(&self) -> bool {
        self.contains(InterruptMask::RX_AVAILABLE)
    }

    pub fn tx_empty(&self) -> bool {
        self.contains(InterruptMask::TX_EMPTY)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub baudrate: Option<u32>,
    pub data_bits: Option<DataBits>,
    pub stop_bits: Option<StopBits>,
    pub parity: Option<Parity>,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn baudrate(mut self, baudrate: u32) -> Self {
        self.baudrate = Some(baudrate);
        self
    }

    pub fn data_bits(mut self, data_bits: DataBits) -> Self {
        self.data_bits = Some(data_bits);
        self
    }

    pub fn stop_bits(mut self, stop_bits: StopBits) -> Self {
        self.stop_bits = Some(stop_bits);
        self
    }

    pub fn parity(mut self, parity: Parity) -> Self {
        self.parity = Some(parity);
        self
    }
}

pub trait InterfaceRaw: Send + Any + 'static {
    type IrqHandler: TIrqHandler;
    type Sender: TSender;
    type Receiver: TReceiver;

    fn name(&self) -> &str;

    fn base_addr(&self) -> usize;

    // ==================== 配置管理 ====================
    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn baudrate(&self) -> u32;
    fn data_bits(&self) -> DataBits;
    fn stop_bits(&self) -> StopBits;
    fn parity(&self) -> Parity;
    fn clock_freq(&self) -> Option<NonZeroU32>;

    fn open(&mut self);
    fn close(&mut self);

    // ==================== 回环控制 ====================
    /// 启用回环模式
    fn enable_loopback(&mut self);
    /// 禁用回环模式
    fn disable_loopback(&mut self);
    /// 检查回环模式是否启用
    fn is_loopback_enabled(&self) -> bool;

    // ==================== 中断管理 ====================
    /// 设置中断使能掩码
    fn set_irq_mask(&mut self, mask: InterruptMask);
    /// 获取当前中断使能掩码
    fn get_irq_mask(&self) -> InterruptMask;

    fn irq_handler(&mut self) -> Option<Self::IrqHandler>;
    fn take_tx(&mut self) -> Option<Self::Sender>;
    fn take_rx(&mut self) -> Option<Self::Receiver>;

    fn set_tx(&mut self, tx: Self::Sender) -> Result<(), SetBackError>;
    fn set_rx(&mut self, rx: Self::Receiver) -> Result<(), SetBackError>;
}

#[derive(Clone, Copy)]
pub struct SetBackError {
    want: usize,
    actual: usize,
}

impl SetBackError {
    /// Create a new SetBackError
    /// # Safety
    pub fn new(want: usize, actual: usize) -> Self {
        Self {
            want: want as _,
            actual: actual as _,
        }
    }
}

impl core::error::Error for SetBackError {}

impl Debug for SetBackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Failed to set back, base address not eq  {:#x} != {:#x}",
            self.want, self.actual
        )
    }
}

impl Display for SetBackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        Debug::fmt(self, f)
    }
}

pub trait Interface: DriverGeneric {
    fn irq_handler(&mut self) -> Option<Box<dyn TIrqHandler>>;
    fn take_tx(&mut self) -> Option<Box<dyn TSender>>;
    fn take_rx(&mut self) -> Option<Box<dyn TReceiver>>;
    /// Base address of the serial port
    fn base_addr(&self) -> usize;

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn baudrate(&self) -> u32;
    fn data_bits(&self) -> DataBits;
    fn stop_bits(&self) -> StopBits;
    fn parity(&self) -> Parity;
    fn clock_freq(&self) -> Option<NonZeroU32>;

    fn enable_loopback(&mut self);
    fn disable_loopback(&mut self);
    fn is_loopback_enabled(&self) -> bool;

    fn enable_interrupts(&mut self, mask: InterruptMask);
    fn disable_interrupts(&mut self, mask: InterruptMask);
    fn get_enabled_interrupts(&self) -> InterruptMask;
}

pub trait TIrqHandler: Send + Sync + 'static {
    fn clean_interrupt_status(&self) -> InterruptMask;
}

pub trait TSender: Send + 'static {
    fn write_byte(&mut self, byte: u8) -> bool;

    fn write_bytes(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes.iter() {
            if !self.write_byte(byte) {
                break;
            }
            written += 1;
        }
        written
    }
}

pub trait TReceiver: Send + 'static {
    fn read_byte(&mut self) -> Option<Result<u8, TransferError>>;

    /// Recv data into buf, return recv bytes. If return bytes is less than buf.len(), it means no more data.
    fn read_bytes(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let mut read_count = 0;
        for byte in bytes.iter_mut() {
            match self.read_byte() {
                Some(Ok(b)) => {
                    *byte = b;
                }
                Some(Err(e)) => {
                    return Err(TransBytesError {
                        bytes_transferred: read_count,
                        kind: e,
                    });
                }
                None => break,
            }

            read_count += 1;
        }

        Ok(read_count)
    }
}
