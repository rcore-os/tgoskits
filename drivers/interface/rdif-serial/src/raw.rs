use alloc::boxed::Box;
use core::{any::Any, num::NonZeroU32};

use crate::{
    Config, ConfigError, InterruptMask, IrqSnapshot, RxFlag, RxSample, SerialEvent, TransferError,
};

/// Lock-free UART register interface.
///
/// # Concurrency
///
/// Every method must be serialized by the unique outer port owner. An
/// implementation must not introduce OS locks, shared task state, or task
/// wakeup policy.
pub trait RawUart: Send + Any + 'static {
    fn name(&self) -> &'static str;
    fn base_addr(&self) -> usize;
    fn clock_freq(&self) -> Option<NonZeroU32>;

    /// Initializes FIFO, control registers, and line parameters.
    ///
    /// Every device IRQ source must remain disabled on return.
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError>;

    /// Disables every device IRQ source and stops the port.
    fn shutdown(&mut self);

    /// Updates baud rate, data width, parity, and stop bits.
    ///
    /// The unique owner has excluded its local IRQ endpoint and temporarily
    /// masked the device sources before this call.
    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError>;

    fn baudrate(&self) -> u32;
    fn data_bits(&self) -> crate::DataBits;
    fn stop_bits(&self) -> crate::StopBits;
    fn parity(&self) -> crate::Parity;

    fn enable_loopback(&mut self);
    fn disable_loopback(&mut self);
    fn is_loopback_enabled(&self) -> bool;

    /// Writes the device-side IRQ mask; this is not a synchronization primitive.
    fn set_irq_mask(&mut self, mask: InterruptMask);

    /// Reads and acknowledges the current IRQ source as required by hardware.
    fn take_irq_snapshot(&mut self) -> IrqSnapshot;

    /// Reads one RX FIFO sample, or `None` when the FIFO is empty.
    fn read_rx(&mut self) -> Option<RxSample>;

    /// Whether the TX FIFO can accept another byte.
    fn tx_ready(&mut self) -> bool;

    /// Writes one byte into the hardware TX FIFO.
    ///
    /// The caller must first observe `tx_ready()`.
    fn write_tx(&mut self, byte: u8);

    /// Read a raw hardware status snapshot.
    ///
    /// This is for polling users that directly own the raw UART, such as
    /// someboot early console. Runtime TX/RX queues must not call this method.
    fn poll_status(&mut self) -> SerialEvent;

    /// Direct polling helper for early console users.
    fn write_byte(&mut self, byte: u8) {
        self.write_tx(byte);
    }

    /// Consume one byte/error according to caller-owned polling state.
    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        if !status.rx_ready() && !status.rx_error() {
            return None;
        }
        let sample = self.read_rx()?;
        if sample.overrun {
            return Some(Err(TransferError::Overrun(sample.byte.unwrap_or(0))));
        }
        let byte = sample.byte?;
        match sample.flag {
            RxFlag::Normal => Some(Ok(byte)),
            RxFlag::Break => Some(Err(TransferError::Break)),
            RxFlag::Parity => Some(Err(TransferError::Parity)),
            RxFlag::Framing => Some(Err(TransferError::Framing)),
        }
    }

    /// Maximum number of bytes that may be loaded after one TX-ready fact.
    ///
    /// Implementations must return a non-zero value. The runtime treats zero
    /// as a driver invariant failure and disables the device interrupt source.
    fn tx_load_size(&self) -> usize {
        1
    }

    /// Whether both the FIFO and shift register are empty.
    fn tx_idle(&mut self) -> bool;

    fn ack_modem_status(&mut self) {}
    fn ack_busy_detect(&mut self) {}
}

impl RawUart for Box<dyn RawUart> {
    fn name(&self) -> &'static str {
        self.as_ref().name()
    }

    fn base_addr(&self) -> usize {
        self.as_ref().base_addr()
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        self.as_ref().clock_freq()
    }

    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.as_mut().startup(config)
    }

    fn shutdown(&mut self) {
        self.as_mut().shutdown();
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.as_mut().set_config(config)
    }

    fn baudrate(&self) -> u32 {
        self.as_ref().baudrate()
    }

    fn data_bits(&self) -> crate::DataBits {
        self.as_ref().data_bits()
    }

    fn stop_bits(&self) -> crate::StopBits {
        self.as_ref().stop_bits()
    }

    fn parity(&self) -> crate::Parity {
        self.as_ref().parity()
    }

    fn enable_loopback(&mut self) {
        self.as_mut().enable_loopback();
    }

    fn disable_loopback(&mut self) {
        self.as_mut().disable_loopback();
    }

    fn is_loopback_enabled(&self) -> bool {
        self.as_ref().is_loopback_enabled()
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.as_mut().set_irq_mask(mask);
    }

    fn take_irq_snapshot(&mut self) -> IrqSnapshot {
        self.as_mut().take_irq_snapshot()
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        self.as_mut().read_rx()
    }

    fn tx_ready(&mut self) -> bool {
        self.as_mut().tx_ready()
    }

    fn write_tx(&mut self, byte: u8) {
        self.as_mut().write_tx(byte);
    }

    fn poll_status(&mut self) -> SerialEvent {
        self.as_mut().poll_status()
    }

    fn write_byte(&mut self, byte: u8) {
        self.as_mut().write_byte(byte);
    }

    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        self.as_mut().read_byte(status)
    }

    fn tx_load_size(&self) -> usize {
        self.as_ref().tx_load_size()
    }

    fn tx_idle(&mut self) -> bool {
        self.as_mut().tx_idle()
    }

    fn ack_modem_status(&mut self) {
        self.as_mut().ack_modem_status();
    }

    fn ack_busy_detect(&mut self) {
        self.as_mut().ack_busy_detect();
    }
}
