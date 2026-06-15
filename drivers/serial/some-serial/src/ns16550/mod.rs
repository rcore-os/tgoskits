//! NS16550/16450 UART 驱动模块
//!
//! 提供两种访问方式：
//! - IO Port 版本（x86_64 架构）
//! - MMIO 版本（通用嵌入式平台）

extern crate alloc;

// 公共寄存器定义
mod registers;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use bitflags::Flags;
use rdif_serial::{
    Config, ConfigError, DataBits, InterfaceRaw, InterruptMask, Parity, SerialDirection,
    SerialEvent, StopBits, TIrqHandler, TRxQueue, TTxQueue, TransBytesError, TransferError,
};
use registers::*;

pub mod dw_apb;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod pio;
pub mod rockchip_fiq;
// MMIO 版本（通用）
mod mmio;

pub use dw_apb::*;
pub use mmio::*;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use pio::*;
pub use rockchip_fiq::*;

pub trait Kind: Clone + Send + Sync + 'static {
    fn read_reg(&self, reg: u8) -> u8;
    fn write_reg(&self, reg: u8, val: u8);
    fn get_base(&self) -> usize;

    fn set_baudrate(&self, clock_freq: u32, baudrate: u32) -> Result<(), ConfigError> {
        if baudrate == 0 || clock_freq == 0 {
            return Err(ConfigError::InvalidBaudrate);
        }

        let divisor = clock_freq / (16 * baudrate);
        if divisor == 0 || divisor > 0xFFFF {
            return Err(ConfigError::InvalidBaudrate);
        }

        let mut lcr: LineControlFlags = self.read_flags(UART_LCR);
        lcr.insert(LineControlFlags::DIVISOR_LATCH_ACCESS);
        self.write_flags(UART_LCR, lcr);

        self.write_reg(UART_DLL, (divisor & 0xFF) as u8);
        self.write_reg(UART_DLH, ((divisor >> 8) & 0xFF) as u8);

        lcr.remove(LineControlFlags::DIVISOR_LATCH_ACCESS);
        self.write_flags(UART_LCR, lcr);

        Ok(())
    }

    fn baudrate(&self, clock_freq: u32) -> u32 {
        let dll = self.read_reg(UART_DLL) as u16;
        let dlh = self.read_reg(UART_DLH) as u16;
        let divisor = dll | (dlh << 8);

        if divisor == 0 {
            return 0;
        }

        clock_freq / (16 * divisor as u32)
    }

    fn init(&self) {
        self.write_flags(UART_IER, InterruptEnableFlags::empty());

        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.insert(ModemControlFlags::DATA_TERMINAL_READY | ModemControlFlags::REQUEST_TO_SEND);
        self.write_flags(UART_MCR, mcr);
    }

    // 类型安全的 bitflags 寄存器访问
    fn read_flags<F: Flags<Bits = u8>>(&self, reg: u8) -> F {
        F::from_bits_retain(self.read_reg(reg))
    }

    fn write_flags<F: Flags<Bits = u8>>(&self, reg: u8, val: F) {
        self.write_reg(reg, val.bits());
    }
}

pub struct Ns16550<T: Kind> {
    pub(crate) base: T,
    pub(crate) clock_freq: u32,
    pub(crate) saved_lsr: LineStatusFlags,
}

impl<T: Kind> InterfaceRaw for Ns16550<T> {
    type SharedState = Ns16550SharedState;
    type TxQueue = Ns16550TxQueue<T>;
    type RxQueue = Ns16550RxQueue<T>;
    type IrqHandler = Ns16550IrqHandler<T>;

    fn name(&self) -> &str {
        "NS16550 UART"
    }

    fn base_addr(&self) -> usize {
        self.base.get_base()
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        // 配置波特率
        if let Some(baudrate) = config.baudrate {
            self.set_baudrate_internal(baudrate)?;
        }

        // 配置数据位
        if let Some(data_bits) = config.data_bits {
            self.set_data_bits_internal(data_bits)?;
        }

        // 配置停止位
        if let Some(stop_bits) = config.stop_bits {
            self.set_stop_bits_internal(stop_bits)?;
        }

        // 配置奇偶校验
        if let Some(parity) = config.parity {
            self.set_parity_internal(parity)?;
        }
        Ok(())
    }

    fn baudrate(&self) -> u32 {
        self.base.baudrate(self.clock_freq)
    }

    fn data_bits(&self) -> DataBits {
        let lcr: LineControlFlags = self.read_flags(UART_LCR);
        let wlen = lcr & LineControlFlags::WORD_LENGTH_MASK;
        if wlen == LineControlFlags::WORD_LENGTH_5 {
            DataBits::Five
        } else if wlen == LineControlFlags::WORD_LENGTH_6 {
            DataBits::Six
        } else if wlen == LineControlFlags::WORD_LENGTH_7 {
            DataBits::Seven
        } else {
            DataBits::Eight // 默认值
        }
    }

    fn stop_bits(&self) -> StopBits {
        let lcr: LineControlFlags = self.read_flags(UART_LCR);
        if lcr.contains(LineControlFlags::STOP_BITS) {
            StopBits::Two
        } else {
            StopBits::One
        }
    }

    fn parity(&self) -> Parity {
        let lcr: LineControlFlags = self.read_flags(UART_LCR);

        if !lcr.contains(LineControlFlags::PARITY_ENABLE) {
            Parity::None
        } else if lcr.contains(LineControlFlags::STICK_PARITY) {
            // Stick parity
            if lcr.contains(LineControlFlags::EVEN_PARITY) {
                Parity::Space
            } else {
                Parity::Mark
            }
        } else {
            // Normal parity
            if lcr.contains(LineControlFlags::EVEN_PARITY) {
                Parity::Even
            } else {
                Parity::Odd
            }
        }
    }

    fn clock_freq(&self) -> Option<core::num::NonZeroU32> {
        self.clock_freq.try_into().ok()
    }

    fn open(&mut self) {
        Ns16550::open(self);
    }

    fn close(&mut self) {
        Ns16550::close(self);
    }

    fn enable_loopback(&mut self) {
        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.insert(ModemControlFlags::LOOPBACK_ENABLE);
        self.write_flags(UART_MCR, mcr);
    }

    fn disable_loopback(&mut self) {
        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.remove(ModemControlFlags::LOOPBACK_ENABLE);
        self.write_flags(UART_MCR, mcr);
    }

    fn is_loopback_enabled(&self) -> bool {
        let mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.contains(ModemControlFlags::LOOPBACK_ENABLE)
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        Ns16550::set_irq_mask(self, mask);
    }

    fn get_irq_mask(&self) -> InterruptMask {
        Ns16550::get_irq_mask(self)
    }

    fn new_shared_state(&self) -> Self::SharedState {
        Ns16550SharedState::new()
    }

    fn tx_queue(&self, shared: &Self::SharedState) -> Self::TxQueue {
        Ns16550TxQueue {
            base: self.base.clone(),
            shared: shared.clone(),
        }
    }

    fn rx_queue(&self, shared: &Self::SharedState) -> Self::RxQueue {
        Ns16550RxQueue {
            base: self.base.clone(),
            shared: shared.clone(),
        }
    }

    fn irq_handler(&self, shared: &Self::SharedState) -> Self::IrqHandler {
        Ns16550IrqHandler {
            base: self.base.clone(),
            shared: shared.clone(),
        }
    }
}

impl<T: Kind> Ns16550<T> {
    // 类型安全的 bitflags 寄存器访问
    fn read_flags<F: Flags<Bits = u8>>(&self, reg: u8) -> F {
        F::from_bits_retain(self.base.read_reg(reg))
    }

    fn write_flags<F: Flags<Bits = u8>>(&mut self, reg: u8, val: F) {
        self.base.write_reg(reg, val.bits());
    }

    pub fn pending(&mut self, direction: SerialDirection) -> bool {
        let lsr = self.read_lsr_preserving();
        match direction {
            SerialDirection::Input => lsr.contains(LineStatusFlags::DATA_READY),
            SerialDirection::Output => lsr.contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY),
        }
    }

    pub fn poll(&mut self) -> SerialEvent {
        serial_event_from_lsr(self.read_lsr_preserving())
    }

    pub fn try_write(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        while written < bytes.len() {
            if !self
                .read_lsr_preserving()
                .contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY)
            {
                break;
            }
            let burst = self.tx_fifo_capacity().min(bytes.len() - written);
            for &byte in &bytes[written..written + burst] {
                self.base.write_reg(UART_THR, byte);
            }
            written += burst;
        }
        written
    }

    pub fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let mut read_count = 0;
        let mut first_error = None;
        for byte in bytes.iter_mut() {
            if !self.pending(SerialDirection::Input) {
                break;
            }
            let result = self.read_byte();
            match result {
                Some(Ok(b)) => {
                    *byte = b;
                    read_count += 1;
                }
                Some(Err(TransferError::Overrun(b))) => {
                    *byte = b;
                    read_count += 1;
                    first_error.get_or_insert(TransferError::Overrun(b));
                }
                Some(Err(e)) => {
                    first_error.get_or_insert(e);
                }
                None => break,
            }
        }
        if let Some(kind) = first_error {
            Err(TransBytesError {
                bytes_transferred: read_count,
                kind,
            })
        } else {
            Ok(read_count)
        }
    }

    pub fn handle_irq(&mut self) -> SerialEvent {
        let iir: InterruptIdentificationFlags = self.read_flags(UART_IIR);
        let mut event = SerialEvent::empty();

        if iir.contains(InterruptIdentificationFlags::NO_INTERRUPT_PENDING) {
            return event;
        }

        let interrupt_id = iir & InterruptIdentificationFlags::INTERRUPT_ID_MASK;
        if interrupt_id == InterruptIdentificationFlags::RECEIVER_LINE_STATUS {
            event |= serial_event_from_lsr(self.read_lsr_preserving())
                & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
            if event.is_empty() {
                event |= SerialEvent::RX_ERROR;
            }
        } else if interrupt_id == InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE
            || interrupt_id == InterruptIdentificationFlags::CHARACTER_TIMEOUT
        {
            event |= SerialEvent::RX_READY;
            event |= serial_event_from_lsr(self.read_lsr_preserving())
                & (SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
        } else if interrupt_id == InterruptIdentificationFlags::TRANSMITTER_HOLDING_EMPTY {
            event |= SerialEvent::TX_READY;
        }

        event
    }

    fn read_lsr_preserving(&mut self) -> LineStatusFlags {
        let lsr: LineStatusFlags = self.read_flags(UART_LSR);
        self.saved_lsr
            .insert(lsr & (LineStatusFlags::ERROR_MASK | LineStatusFlags::FIFO_ERROR));
        lsr | self.saved_lsr
    }

    fn read_byte(&mut self) -> Option<Result<u8, TransferError>> {
        let lsr = self.read_lsr_preserving();

        if lsr.contains(LineStatusFlags::OVERRUN_ERROR) {
            let b = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::OVERRUN_ERROR);
            return Some(Err(TransferError::Overrun(b)));
        }
        if lsr.contains(LineStatusFlags::PARITY_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::PARITY_ERROR);
            return Some(Err(TransferError::Parity));
        }
        if lsr.contains(LineStatusFlags::FRAMING_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::FRAMING_ERROR);
            return Some(Err(TransferError::Framing));
        }
        if lsr.contains(LineStatusFlags::BREAK_INTERRUPT) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::BREAK_INTERRUPT);
            return Some(Err(TransferError::Break));
        }
        if lsr.contains(LineStatusFlags::DATA_READY) {
            return Some(Ok(self.base.read_reg(UART_RBR)));
        }
        None
    }

    pub fn open(&mut self) {
        self.init_core();
    }

    pub fn close(&mut self) {
        self.write_flags(UART_IER, InterruptEnableFlags::empty());

        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.remove(ModemControlFlags::DATA_TERMINAL_READY | ModemControlFlags::REQUEST_TO_SEND);
        self.write_flags(UART_MCR, mcr);
    }

    pub fn set_irq_mask(&mut self, mask: InterruptMask) {
        let mut ier = InterruptEnableFlags::empty();

        if mask.contains(InterruptMask::RX_AVAILABLE) {
            ier.insert(InterruptEnableFlags::RECEIVED_DATA_AVAILABLE);
            ier.insert(InterruptEnableFlags::RECEIVER_LINE_STATUS);
        }
        if mask.contains(InterruptMask::TX_EMPTY) {
            ier.insert(InterruptEnableFlags::TRANSMITTER_HOLDING_EMPTY);
        }

        self.write_flags(UART_IER, ier);
    }

    pub fn get_irq_mask(&self) -> InterruptMask {
        let ier: InterruptEnableFlags = self.read_flags(UART_IER);
        let mut mask = InterruptMask::empty();

        if ier.contains(InterruptEnableFlags::RECEIVED_DATA_AVAILABLE) {
            mask |= InterruptMask::RX_AVAILABLE;
        }
        if ier.contains(InterruptEnableFlags::TRANSMITTER_HOLDING_EMPTY) {
            mask |= InterruptMask::TX_EMPTY;
        }

        mask
    }

    /// 检查是否为 16550+（支持 FIFO）
    pub fn is_16550_plus(&self) -> bool {
        // 通过读取 IIR 寄存器的 FIFO 位来判断
        // IIR 的位7-6在 16550+ 中会显示 FIFO 启用状态
        let fifo: InterruptIdentificationFlags = self.read_flags(UART_IIR);
        fifo.contains(InterruptIdentificationFlags::FIFO_ENABLE_MASK)
    }

    /// 设置波特率
    fn set_baudrate_internal(&mut self, baudrate: u32) -> Result<(), ConfigError> {
        self.base.set_baudrate(self.clock_freq, baudrate)
    }

    /// 设置数据位
    fn set_data_bits_internal(&mut self, bits: DataBits) -> Result<(), ConfigError> {
        let wlen = match bits {
            DataBits::Five => LineControlFlags::WORD_LENGTH_5,
            DataBits::Six => LineControlFlags::WORD_LENGTH_6,
            DataBits::Seven => LineControlFlags::WORD_LENGTH_7,
            DataBits::Eight => LineControlFlags::WORD_LENGTH_8,
        };

        let mut lcr: LineControlFlags = self.read_flags(UART_LCR);
        // 清除旧的数据位设置，然后设置新的
        lcr.remove(LineControlFlags::WORD_LENGTH_MASK);
        lcr.insert(wlen);
        self.write_flags(UART_LCR, lcr);

        Ok(())
    }

    /// 设置停止位
    fn set_stop_bits_internal(&mut self, bits: StopBits) -> Result<(), ConfigError> {
        let mut lcr: LineControlFlags = self.read_flags(UART_LCR);
        match bits {
            StopBits::One => lcr.remove(LineControlFlags::STOP_BITS),
            StopBits::Two => lcr.insert(LineControlFlags::STOP_BITS),
        }
        self.write_flags(UART_LCR, lcr);
        Ok(())
    }

    /// 设置奇偶校验
    fn set_parity_internal(&mut self, parity: Parity) -> Result<(), ConfigError> {
        let mut lcr: LineControlFlags = self.read_flags(UART_LCR);

        // 先清除所有校验相关位
        lcr.remove(
            LineControlFlags::PARITY_ENABLE
                | LineControlFlags::EVEN_PARITY
                | LineControlFlags::STICK_PARITY,
        );

        // 根据校验类型设置相应位
        match parity {
            Parity::None => {
                // 已经清除，无需额外操作
            }
            Parity::Odd => {
                lcr.insert(LineControlFlags::PARITY_ENABLE);
            }
            Parity::Even => {
                lcr.insert(LineControlFlags::PARITY_ENABLE | LineControlFlags::EVEN_PARITY);
            }
            Parity::Mark => {
                lcr.insert(LineControlFlags::PARITY_ENABLE | LineControlFlags::STICK_PARITY);
            }
            Parity::Space => {
                lcr.insert(
                    LineControlFlags::PARITY_ENABLE
                        | LineControlFlags::EVEN_PARITY
                        | LineControlFlags::STICK_PARITY,
                );
            }
        }

        self.write_flags(UART_LCR, lcr);
        Ok(())
    }

    /// 启用或禁用 FIFO
    pub fn enable_fifo(&mut self, enable: bool) {
        if enable && self.is_16550_plus() {
            let mut fcr = FifoControlFlags::ENABLE_FIFO;
            fcr.insert(FifoControlFlags::CLEAR_RECEIVER_FIFO);
            fcr.insert(FifoControlFlags::CLEAR_TRANSMITTER_FIFO);
            fcr.insert(FifoControlFlags::TRIGGER_1_BYTE);
            self.write_flags(UART_FCR, fcr);
        } else {
            self.write_flags(UART_FCR, FifoControlFlags::empty());
        }
    }

    /// 设置 FIFO 触发级别
    pub fn set_fifo_trigger_level(&mut self, level: u8) {
        if !self.is_16550_plus() {
            return;
        }

        let trigger_value = match level {
            0..=3 => FifoControlFlags::TRIGGER_1_BYTE,
            4..=7 => FifoControlFlags::TRIGGER_4_BYTES,
            8..=11 => FifoControlFlags::TRIGGER_8_BYTES,
            _ => FifoControlFlags::TRIGGER_14_BYTES,
        };

        // 读取当前 FCR 设置，清除触发级别位，然后设置新的触发级别
        let mut fcr: FifoControlFlags = self.read_flags(UART_FCR);
        fcr.remove(FifoControlFlags::TRIGGER_LEVEL_MASK);
        fcr.insert(trigger_value);
        self.write_flags(UART_FCR, fcr);
    }

    /// 初始化 UART
    fn init_core(&mut self) {
        self.base.init();
    }

    /// 检查 FIFO 是否启用
    pub fn is_fifo_enabled(&self) -> bool {
        if !self.is_16550_plus() {
            return false;
        }
        // 通过检查 IIR 的 FIFO 位来判断
        let iir: InterruptIdentificationFlags = self.read_flags(UART_IIR);
        iir.contains(InterruptIdentificationFlags::FIFO_ENABLE_MASK)
    }

    fn tx_fifo_capacity(&self) -> usize {
        let iir: InterruptIdentificationFlags = self.read_flags(UART_IIR);
        if iir.contains(InterruptIdentificationFlags::FIFO_ENABLE_MASK) {
            UART_FIFO_SIZE as usize
        } else {
            1
        }
    }
}

#[derive(Clone)]
pub struct Ns16550SharedState {
    event_bits: Arc<AtomicU32>,
    saved_lsr: Arc<AtomicU8>,
}

impl Ns16550SharedState {
    fn new() -> Self {
        Self {
            event_bits: Arc::new(AtomicU32::new(0)),
            saved_lsr: Arc::new(AtomicU8::new(0)),
        }
    }

    fn save_lsr(&self, lsr: LineStatusFlags) -> LineStatusFlags {
        let saved = lsr & (LineStatusFlags::ERROR_MASK | LineStatusFlags::FIFO_ERROR);
        if !saved.is_empty() {
            self.saved_lsr.fetch_or(saved.bits(), Ordering::AcqRel);
        }
        let combined = lsr.bits() | self.saved_lsr.load(Ordering::Acquire);
        LineStatusFlags::from_bits_retain(combined)
    }

    fn clear_lsr(&self, flag: LineStatusFlags) {
        self.saved_lsr.fetch_and(!flag.bits(), Ordering::AcqRel);
    }

    fn push_event(&self, event: SerialEvent) {
        if !event.is_empty() {
            self.event_bits.fetch_or(event.bits(), Ordering::AcqRel);
        }
    }

    fn take_event(&self, mask: SerialEvent) -> SerialEvent {
        let old = self.event_bits.fetch_and(!mask.bits(), Ordering::AcqRel);
        SerialEvent::from_bits_retain(old) & mask
    }

    fn peek_event(&self) -> SerialEvent {
        SerialEvent::from_bits_retain(self.event_bits.load(Ordering::Acquire))
    }
}

pub struct Ns16550TxQueue<T: Kind> {
    base: T,
    shared: Ns16550SharedState,
}

impl<T: Kind> Ns16550TxQueue<T> {
    fn read_lsr_preserving(&self) -> LineStatusFlags {
        let lsr = self.base.read_flags(UART_LSR);
        self.shared.save_lsr(lsr)
    }

    fn tx_fifo_capacity(&self) -> usize {
        let iir: InterruptIdentificationFlags = self.base.read_flags(UART_IIR);
        if iir.contains(InterruptIdentificationFlags::FIFO_ENABLE_MASK) {
            UART_FIFO_SIZE as usize
        } else {
            1
        }
    }
}

impl<T: Kind> TTxQueue for Ns16550TxQueue<T> {
    fn base_addr(&self) -> usize {
        self.base.get_base()
    }

    fn poll(&mut self) -> SerialEvent {
        let event = serial_event_from_lsr(self.read_lsr_preserving()) & SerialEvent::TX_READY;
        self.shared.push_event(event);
        event | (self.shared.peek_event() & SerialEvent::TX_READY)
    }

    fn try_write(&mut self, bytes: &[u8]) -> usize {
        let _ = self.shared.take_event(SerialEvent::TX_READY);
        let mut written = 0;
        while written < bytes.len() {
            if !self
                .read_lsr_preserving()
                .contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY)
            {
                break;
            }
            let burst = self.tx_fifo_capacity().min(bytes.len() - written);
            for &byte in &bytes[written..written + burst] {
                self.base.write_reg(UART_THR, byte);
            }
            written += burst;
        }
        written
    }
}

pub struct Ns16550RxQueue<T: Kind> {
    base: T,
    shared: Ns16550SharedState,
}

impl<T: Kind> Ns16550RxQueue<T> {
    fn read_lsr_preserving(&self) -> LineStatusFlags {
        let lsr = self.base.read_flags(UART_LSR);
        self.shared.save_lsr(lsr)
    }

    fn read_byte(&self) -> Option<Result<u8, TransferError>> {
        let lsr = self.read_lsr_preserving();

        if lsr.contains(LineStatusFlags::OVERRUN_ERROR) {
            let b = self.base.read_reg(UART_RBR);
            self.shared.clear_lsr(LineStatusFlags::OVERRUN_ERROR);
            return Some(Err(TransferError::Overrun(b)));
        }
        if lsr.contains(LineStatusFlags::PARITY_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.shared.clear_lsr(LineStatusFlags::PARITY_ERROR);
            return Some(Err(TransferError::Parity));
        }
        if lsr.contains(LineStatusFlags::FRAMING_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.shared.clear_lsr(LineStatusFlags::FRAMING_ERROR);
            return Some(Err(TransferError::Framing));
        }
        if lsr.contains(LineStatusFlags::BREAK_INTERRUPT) {
            let _ = self.base.read_reg(UART_RBR);
            self.shared.clear_lsr(LineStatusFlags::BREAK_INTERRUPT);
            return Some(Err(TransferError::Break));
        }
        if lsr.contains(LineStatusFlags::DATA_READY) {
            return Some(Ok(self.base.read_reg(UART_RBR)));
        }
        None
    }
}

impl<T: Kind> TRxQueue for Ns16550RxQueue<T> {
    fn base_addr(&self) -> usize {
        self.base.get_base()
    }

    fn poll(&mut self) -> SerialEvent {
        let event = serial_event_from_lsr(self.read_lsr_preserving())
            & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
        self.shared.push_event(event);
        event
            | (self.shared.peek_event()
                & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN))
    }

    fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let _ = self
            .shared
            .take_event(SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
        let mut read_count = 0;
        let mut first_error = None;
        for byte in bytes.iter_mut() {
            if !self
                .read_lsr_preserving()
                .contains(LineStatusFlags::DATA_READY)
                && self.shared.saved_lsr.load(Ordering::Acquire)
                    & LineStatusFlags::ERROR_MASK.bits()
                    == 0
            {
                break;
            }
            let result = self.read_byte();
            match result {
                Some(Ok(b)) => {
                    *byte = b;
                    read_count += 1;
                }
                Some(Err(TransferError::Overrun(b))) => {
                    *byte = b;
                    read_count += 1;
                    first_error.get_or_insert(TransferError::Overrun(b));
                }
                Some(Err(e)) => {
                    first_error.get_or_insert(e);
                }
                None => break,
            }
        }
        if let Some(kind) = first_error {
            Err(TransBytesError {
                bytes_transferred: read_count,
                kind,
            })
        } else {
            Ok(read_count)
        }
    }
}

pub struct Ns16550IrqHandler<T: Kind> {
    base: T,
    shared: Ns16550SharedState,
}

impl<T: Kind> TIrqHandler for Ns16550IrqHandler<T> {
    fn base_addr(&self) -> usize {
        self.base.get_base()
    }

    fn handle_irq(&self) -> SerialEvent {
        let iir: InterruptIdentificationFlags = self.base.read_flags(UART_IIR);
        let mut event = SerialEvent::empty();

        if iir.contains(InterruptIdentificationFlags::NO_INTERRUPT_PENDING) {
            return event;
        }

        let interrupt_id = iir & InterruptIdentificationFlags::INTERRUPT_ID_MASK;
        if interrupt_id == InterruptIdentificationFlags::RECEIVER_LINE_STATUS {
            event |= serial_event_from_lsr(self.shared.save_lsr(self.base.read_flags(UART_LSR)))
                & (SerialEvent::RX_READY | SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
            if event.is_empty() {
                event |= SerialEvent::RX_ERROR;
            }
        } else if interrupt_id == InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE
            || interrupt_id == InterruptIdentificationFlags::CHARACTER_TIMEOUT
        {
            event |= SerialEvent::RX_READY;
            event |= serial_event_from_lsr(self.shared.save_lsr(self.base.read_flags(UART_LSR)))
                & (SerialEvent::RX_ERROR | SerialEvent::OVERRUN);
        } else if interrupt_id == InterruptIdentificationFlags::TRANSMITTER_HOLDING_EMPTY {
            event |= SerialEvent::TX_READY;
        }

        self.shared.push_event(event);
        event
    }
}

fn serial_event_from_lsr(lsr: LineStatusFlags) -> SerialEvent {
    let mut event = SerialEvent::empty();
    if lsr.contains(LineStatusFlags::DATA_READY) {
        event |= SerialEvent::RX_READY;
    }
    if lsr.intersects(
        LineStatusFlags::PARITY_ERROR
            | LineStatusFlags::FRAMING_ERROR
            | LineStatusFlags::BREAK_INTERRUPT,
    ) {
        event |= SerialEvent::RX_ERROR;
    }
    if lsr.contains(LineStatusFlags::OVERRUN_ERROR) {
        event |= SerialEvent::RX_ERROR | SerialEvent::OVERRUN;
    }
    if lsr.contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY) {
        event |= SerialEvent::TX_READY;
    }
    event
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static REGS: [AtomicU8; 8] = [const { AtomicU8::new(0) }; 8];
    static THR_WRITES: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone)]
    struct MockKind;

    impl Kind for MockKind {
        fn read_reg(&self, reg: u8) -> u8 {
            let value = REGS[reg as usize].load(Ordering::SeqCst);
            if reg == UART_RBR {
                REGS[UART_LSR as usize]
                    .fetch_and(!LineStatusFlags::ERROR_MASK.bits(), Ordering::SeqCst);
            }
            value
        }

        fn write_reg(&self, reg: u8, val: u8) {
            REGS[reg as usize].store(val, Ordering::SeqCst);
            if reg == UART_THR {
                let iir = REGS[UART_IIR as usize].load(Ordering::SeqCst);
                if iir & InterruptIdentificationFlags::FIFO_ENABLE_MASK.bits() == 0 {
                    REGS[UART_LSR as usize].fetch_and(
                        !LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
                        Ordering::SeqCst,
                    );
                } else {
                    let writes = THR_WRITES.fetch_add(1, Ordering::SeqCst) + 1;
                    if writes >= UART_FIFO_SIZE as usize {
                        REGS[UART_LSR as usize].fetch_and(
                            !LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
                            Ordering::SeqCst,
                        );
                    }
                }
            }
        }

        fn get_base(&self) -> usize {
            0x1000
        }
    }

    fn reset_regs() {
        for reg in &REGS {
            reg.store(0, Ordering::SeqCst);
        }
        THR_WRITES.store(0, Ordering::SeqCst);
    }

    fn serial() -> (MutexGuard<'static, ()>, Ns16550<MockKind>) {
        let guard = TEST_LOCK.lock().unwrap();
        reset_regs();
        (
            guard,
            Ns16550 {
                base: MockKind,
                clock_freq: 1_843_200,
                saved_lsr: LineStatusFlags::empty(),
            },
        )
    }

    #[test]
    fn pending_output_preserves_rx_error_latch() {
        let (_guard, mut uart) = serial();
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::TRANSMITTER_HOLDING_EMPTY | LineStatusFlags::PARITY_ERROR).bits(),
            Ordering::SeqCst,
        );

        assert!(uart.pending(SerialDirection::Output));

        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        let mut buf = [0];
        let err = uart
            .try_read(&mut buf)
            .expect_err("saved parity error should be reported by next read");
        assert_eq!(err.bytes_transferred, 0);
        assert_eq!(err.kind, TransferError::Parity);
    }

    #[test]
    fn try_write_stops_when_tx_fifo_becomes_full() {
        let (_guard, mut uart) = serial();
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );

        assert_eq!(uart.try_write(b"ab"), 1);
        assert_eq!(REGS[UART_THR as usize].load(Ordering::SeqCst), b'a');
    }

    #[test]
    fn try_write_fills_enabled_tx_fifo_in_one_pass() {
        let (_guard, mut uart) = serial();
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::FIFO_ENABLE_MASK.bits(),
            Ordering::SeqCst,
        );

        assert_eq!(uart.try_write(b"abcdefghijklmnopq"), 16);
        assert_eq!(REGS[UART_THR as usize].load(Ordering::SeqCst), b'p');
    }

    #[test]
    fn try_read_empty_returns_zero() {
        let (_guard, mut uart) = serial();
        let mut buf = [0];

        assert_eq!(uart.try_read(&mut buf), Ok(0));
    }

    #[test]
    fn handle_irq_saves_rx_error_for_task_read() {
        let (_guard, mut uart) = serial();
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::OVERRUN_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(0xab, Ordering::SeqCst);

        let event = uart.handle_irq();
        assert!(event.intersects(SerialEvent::RX_ERROR | SerialEvent::OVERRUN));

        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        let mut buf = [0];
        let err = uart
            .try_read(&mut buf)
            .expect_err("saved overrun should be reported by task read");
        assert_eq!(buf[0], 0xab);
        assert_eq!(err.bytes_transferred, 1);
        assert_eq!(err.kind, TransferError::Overrun(0xab));
    }

    #[test]
    fn split_parts_share_atomic_state_without_shared_raw_struct() {
        let (_guard, uart) = serial();
        let shared = uart.new_shared_state();
        let mut tx = uart.tx_queue(&shared);
        let mut rx = uart.rx_queue(&shared);
        let irq = uart.irq_handler(&shared);

        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        assert_eq!(tx.try_write(b"ab"), 1);
        assert_eq!(REGS[UART_THR as usize].load(Ordering::SeqCst), b'a');

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        REGS[UART_RBR as usize].store(b'z', Ordering::SeqCst);
        assert!(irq.handle_irq().rx_ready());

        let mut buf = [0];
        assert_eq!(rx.try_read(&mut buf), Ok(1));
        assert_eq!(buf[0], b'z');
    }

    #[test]
    fn split_irq_saved_lsr_error_is_consumed_by_rx_queue() {
        let (_guard, uart) = serial();
        let shared = uart.new_shared_state();
        let irq = uart.irq_handler(&shared);
        let mut rx = uart.rx_queue(&shared);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::PARITY_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(b'p', Ordering::SeqCst);

        let event = irq.handle_irq();
        assert!(event.rx_error());

        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        let mut buf = [0];
        let err = rx
            .try_read(&mut buf)
            .expect_err("split RX should consume saved IRQ-side parity error");
        assert_eq!(err.bytes_transferred, 0);
        assert_eq!(err.kind, TransferError::Parity);
    }

    #[test]
    fn split_rx_overrun_returns_current_byte_to_caller() {
        let (_guard, uart) = serial();
        let shared = uart.new_shared_state();
        let irq = uart.irq_handler(&shared);
        let mut rx = uart.rx_queue(&shared);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::OVERRUN_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(b'S', Ordering::SeqCst);

        let event = irq.handle_irq();
        assert!(event.intersects(SerialEvent::RX_ERROR | SerialEvent::OVERRUN));

        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        let mut buf = [0];
        let err = rx
            .try_read(&mut buf)
            .expect_err("overrun should still be reported");
        assert_eq!(buf[0], b'S');
        assert_eq!(err.bytes_transferred, 1);
        assert_eq!(err.kind, TransferError::Overrun(b'S'));
    }

    #[test]
    fn split_rx_overrun_continues_drain_after_error_byte() {
        let (_guard, uart) = serial();
        let shared = uart.new_shared_state();
        let mut rx = uart.rx_queue(&shared);

        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::OVERRUN_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(b'S', Ordering::SeqCst);

        let mut buf = [0; 2];
        let err = rx
            .try_read(&mut buf)
            .expect_err("overrun should still be reported after draining");
        assert_eq!(buf, [b'S', b'S']);
        assert_eq!(err.bytes_transferred, 2);
        assert_eq!(err.kind, TransferError::Overrun(b'S'));
    }

    #[test]
    fn rx_read_does_not_consume_tx_ready_event() {
        let (_guard, uart) = serial();
        let shared = uart.new_shared_state();
        let mut tx = uart.tx_queue(&shared);
        let mut rx = uart.rx_queue(&shared);

        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        assert!(tx.poll().tx_ready());

        let mut buf = [0];
        assert_eq!(rx.try_read(&mut buf), Ok(0));

        REGS[UART_LSR as usize].store(0, Ordering::SeqCst);
        assert!(
            tx.poll().tx_ready(),
            "RX must not clear a TX_READY event owned by the TX queue"
        );
    }
}
