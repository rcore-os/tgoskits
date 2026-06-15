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
pub type BTxQueue = Box<dyn TTxQueue>;
pub type BRxQueue = Box<dyn TRxQueue>;
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

bitflags! {
    /// Stable serial events returned by poll and IRQ paths.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SerialEvent: u32 {
        const RX_READY = 0x01;
        const TX_READY = 0x02;
        const RX_ERROR = 0x04;
        const TX_ERROR = 0x08;
        const OVERRUN = 0x10;
    }
}

impl SerialEvent {
    pub fn rx_ready(&self) -> bool {
        self.contains(SerialEvent::RX_READY)
    }

    pub fn tx_ready(&self) -> bool {
        self.contains(SerialEvent::TX_READY)
    }

    pub fn rx_error(&self) -> bool {
        self.intersects(SerialEvent::RX_ERROR | SerialEvent::OVERRUN)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialDirection {
    Input,
    Output,
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

    fn poll(&mut self) -> SerialEvent;
    fn try_write(&mut self, bytes: &[u8]) -> usize;
    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError>;
    fn handle_irq(&mut self) -> SerialEvent;
}

#[derive(Clone, Copy)]
pub struct SetBackError {
    want: usize,
    actual: usize,
}

impl SetBackError {
    pub fn new(want: usize, actual: usize) -> Self {
        Self { want, actual }
    }

    pub fn want(&self) -> usize {
        self.want
    }

    pub fn actual(&self) -> usize {
        self.actual
    }
}

impl core::error::Error for SetBackError {}

impl Debug for SetBackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "failed to restore serial handle: base address mismatch {:#x} != {:#x}",
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
    /// Base address of the serial port
    fn base_addr(&self) -> usize;

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn baudrate(&self) -> u32;
    fn data_bits(&self) -> DataBits;
    fn stop_bits(&self) -> StopBits;
    fn parity(&self) -> Parity;
    fn clock_freq(&self) -> Option<NonZeroU32>;

    fn open(&mut self);
    fn close(&mut self);

    fn enable_loopback(&mut self);
    fn disable_loopback(&mut self);
    fn is_loopback_enabled(&self) -> bool;

    fn set_irq_mask(&mut self, mask: InterruptMask);
    fn get_irq_mask(&self) -> InterruptMask;

    fn take_tx(&mut self) -> Option<BTxQueue>;
    fn take_rx(&mut self) -> Option<BRxQueue>;
    fn take_irq_handler(&mut self) -> Option<BIrqHandler>;

    fn set_tx(&mut self, tx: BTxQueue) -> Result<(), SetBackError>;
    fn set_rx(&mut self, rx: BRxQueue) -> Result<(), SetBackError>;
    fn set_irq_handler(&mut self, irq: BIrqHandler) -> Result<(), SetBackError>;
}

pub trait TTxQueue: Send + 'static {
    fn base_addr(&self) -> usize;
    fn poll(&mut self) -> SerialEvent;
    fn try_write(&mut self, bytes: &[u8]) -> usize;
}

pub trait TRxQueue: Send + 'static {
    fn base_addr(&self) -> usize;
    fn poll(&mut self) -> SerialEvent;
    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError>;
}

pub trait TIrqHandler: Send + Sync + 'static {
    fn base_addr(&self) -> usize;
    fn handle_irq(&self) -> SerialEvent;
}

#[cfg(test)]
mod tests {
    use core::{
        num::NonZeroU32,
        sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn serial_event_reports_readiness_and_errors() {
        let event = SerialEvent::RX_READY | SerialEvent::OVERRUN;

        assert!(event.rx_ready());
        assert!(!event.tx_ready());
        assert!(event.rx_error());
    }

    #[test]
    fn split_handle_drop_restores_take_lifecycle() {
        let mut serial = SerialDyn::new_boxed(MockSerial::new(0x1000));

        let tx = serial.take_tx().expect("first TX take should work");
        assert!(serial.take_tx().is_none());
        drop(tx);
        assert!(serial.take_tx().is_some());

        let rx = serial.take_rx().expect("first RX take should work");
        assert!(serial.take_rx().is_none());
        drop(rx);
        assert!(serial.take_rx().is_some());

        let irq = serial
            .take_irq_handler()
            .expect("first IRQ handler take should work");
        assert!(serial.take_irq_handler().is_none());
        drop(irq);
        assert!(serial.take_irq_handler().is_some());
    }

    #[test]
    fn split_restore_rejects_base_mismatch() {
        let mut left = SerialDyn::new_boxed(MockSerial::new(0x1000));
        let mut right = SerialDyn::new_boxed(MockSerial::new(0x2000));
        let tx = left.take_tx().expect("TX handle should be available");

        let err = right
            .set_tx(tx)
            .expect_err("wrong serial should reject restored TX handle");
        assert_eq!(err.want(), 0x2000);
        assert_eq!(err.actual(), 0x1000);
    }

    #[test]
    fn split_queues_forward_try_io_to_shared_raw_device() {
        let mut serial = SerialDyn::new_boxed(MockSerial::new(0x1000));
        let mut tx = serial.take_tx().expect("TX handle should be available");
        let mut rx = serial.take_rx().expect("RX handle should be available");

        let written = tx.try_write(b"abc");
        assert_eq!(written, 3);

        let mut buf = [0; 4];
        let read = rx.try_read(&mut buf).expect("RX read should succeed");
        assert_eq!(read, 3);
        assert_eq!(&buf[..read], b"abc");
    }

    struct MockSerial {
        base: usize,
        packed: AtomicU32,
        bytes: AtomicUsize,
        irq_count: AtomicUsize,
    }

    impl MockSerial {
        fn new(base: usize) -> Self {
            Self {
                base,
                packed: AtomicU32::new(0),
                bytes: AtomicUsize::new(0),
                irq_count: AtomicUsize::new(0),
            }
        }
    }

    impl InterfaceRaw for MockSerial {
        fn name(&self) -> &str {
            "mock serial"
        }

        fn base_addr(&self) -> usize {
            self.base
        }

        fn set_config(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn baudrate(&self) -> u32 {
            115_200
        }

        fn data_bits(&self) -> DataBits {
            DataBits::Eight
        }

        fn stop_bits(&self) -> StopBits {
            StopBits::One
        }

        fn parity(&self) -> Parity {
            Parity::None
        }

        fn clock_freq(&self) -> Option<NonZeroU32> {
            NonZeroU32::new(1_843_200)
        }

        fn open(&mut self) {}

        fn close(&mut self) {}

        fn enable_loopback(&mut self) {}

        fn disable_loopback(&mut self) {}

        fn is_loopback_enabled(&self) -> bool {
            false
        }

        fn set_irq_mask(&mut self, _mask: InterruptMask) {}

        fn get_irq_mask(&self) -> InterruptMask {
            InterruptMask::empty()
        }

        fn poll(&mut self) -> SerialEvent {
            SerialEvent::TX_READY | SerialEvent::RX_READY
        }

        fn try_write(&mut self, bytes: &[u8]) -> usize {
            let mut packed = 0;
            let written = bytes.len().min(4);
            for (index, byte) in bytes.iter().copied().take(written).enumerate() {
                packed |= (byte as u32) << (index * 8);
            }
            self.packed.store(packed, Ordering::Release);
            self.bytes.store(written, Ordering::Release);
            written
        }

        fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
            let available = self.bytes.swap(0, Ordering::AcqRel);
            let read = available.min(bytes.len());
            let packed = self.packed.load(Ordering::Acquire);
            for (index, out) in bytes.iter_mut().take(read).enumerate() {
                *out = ((packed >> (index * 8)) & 0xff) as u8;
            }
            Ok(read)
        }

        fn handle_irq(&mut self) -> SerialEvent {
            self.irq_count.fetch_add(1, Ordering::AcqRel);
            SerialEvent::TX_READY
        }
    }
}
