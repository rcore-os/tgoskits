#![no_std]

//! # Some Serial - 嵌入式串口驱动集合
//!
//! 本库提供统一的串口驱动接口，支持多种硬件平台：
//! - ARM PL011 UART
//! - NS16550/16450 UART（IO Port、MMIO 和 DesignWare APB 版本）
//!
//! ## 特性
//!
//! - 🏗️ 统一抽象接口 - 驱动层只提供 UART 寄存器语义，运行期队列由 OS runtime 提供
//! - 🛡️ 无标准库设计 (`no_std`) - 适用于裸机和嵌入式系统
//! - 📦 模块化架构 - 每个驱动独立模块，按需选择
//! - 🔒 类型安全 - 使用 Rust 类型系统确保内存安全
//! - ⚡ 高性能 - 零拷贝数据传输，直接硬件访问
//!
//! ## 支持的驱动
//!
//! ### ARM PL011 UART
//! - 广泛用于 ARM Cortex-A、Cortex-M、Cortex-R 系列
//! - 支持 FIFO、中断、回环等完整功能
//!
//! ### NS16550/16450 UART
//! - 经典 PC 串口控制器，广泛兼容
//! - 支持 IO Port（x86_64）、MMIO（通用）和 DesignWare APB 访问方式
//! - 支持 16 字节 FIFO 缓冲
//!
//! ## 快速开始
//!
//! ```rust,no_run
//! use core::ptr::NonNull;
//!
//! use some_serial::{Config, PollingUart as _, UartPort as _, ns16550::Ns16550, pl011::Pl011};
//!
//! // 选择合适的驱动
//! #[cfg(target_arch = "aarch64")]
//! let mut uart = Pl011::new(NonNull::new(0x9000000 as *mut u8).unwrap(), 24_000_000);
//!
//! #[cfg(not(target_arch = "aarch64"))]
//! let mut uart = Ns16550::new_mmio(NonNull::new(0x9000000 as *mut u8).unwrap(), 1_843_200, 1);
//!
//! // 配置串口
//! let config = Config::new()
//!     .baudrate(115200)
//!     .data_bits(some_serial::DataBits::Eight)
//!     .stop_bits(some_serial::StopBits::One)
//!     .parity(some_serial::Parity::None);
//!
//! uart.startup(&config).unwrap();
//!
//! while !uart.poll_status().tx_ready() {
//!     core::hint::spin_loop();
//! }
//! uart.write_byte(b'h');
//! ```

#[cfg(test)]
extern crate std;

pub mod ns16550;
pub mod pl011;

use core::fmt::Display;

use bitflags::bitflags;

/// Allocation-free polling interface used by early consoles.
pub trait PollingUart {
    fn poll_status(&mut self) -> PollingEvent;

    fn write_byte(&mut self, byte: u8);

    fn read_byte(&mut self, status: PollingEvent) -> Option<Result<u8, TransferError>>;
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PollingEvent: u32 {
        const RX_READY = 0x01;
        const TX_READY = 0x02;
        const RX_ERROR = 0x04;
        const TX_ERROR = 0x08;
        const OVERRUN = 0x10;
        const MODEM_STATUS = 0x20;
    }
}

impl PollingEvent {
    pub const fn rx_ready(self) -> bool {
        self.contains(Self::RX_READY)
    }

    pub const fn tx_ready(self) -> bool {
        self.contains(Self::TX_READY)
    }

    pub const fn rx_error(self) -> bool {
        self.intersects(Self::RX_ERROR.union(Self::OVERRUN))
    }
}

pub type SerialEvent = PollingEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialDirection {
    Input,
    Output,
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    #[error("data overrun by `{0:#x}`")]
    Overrun(u8),
    #[error("parity error")]
    Parity,
    #[error("framing error")]
    Framing,
    #[error("break condition")]
    Break,
    #[error("serial closed")]
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransBytesError {
    pub bytes_transferred: usize,
    pub kind: TransferError,
}

impl Display for TransBytesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "transfer error after transferring {} bytes: {}",
            self.bytes_transferred, self.kind
        )
    }
}

impl core::error::Error for TransBytesError {}

// Runtime capability types are re-exported for concrete driver consumers.
pub use rdif_serial::*;
