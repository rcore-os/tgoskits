//! NS16550/16450 UART 驱动模块
//!
//! 提供两种访问方式：
//! - IO Port 版本（x86_64 架构）
//! - MMIO 版本（通用嵌入式平台）

// 公共寄存器定义
mod registers;

use bitflags::Flags;
use rdif_serial::{
    Config, ConfigError, DataBits, IRQ_RX_BATCH_CAPACITY, IrqRxBatch, Parity, RxErrorFlags, RxFlag,
    RxSample, SerialEventSet, SerialIrqEvent, SerialIrqReport, SerialParts, SplitUart, StopBits,
    UartEmergencyTx, UartInfo, UartIrq, UartPort,
};
use registers::*;

use crate::{PollingUart, SerialDirection, SerialEvent, TransBytesError, TransferError};

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

    fn ack_busy_detect(&self) {}

    /// Programs the divisor after validating all fallible parameters.
    /// Implementations must not modify registers when returning `Err`.
    fn set_baudrate(&self, clock_freq: u32, baudrate: u32) -> Result<(), ConfigError> {
        if baudrate == 0 || clock_freq == 0 {
            return Err(ConfigError::InvalidBaudrate);
        }

        let divisor = clock_freq / (16 * baudrate);
        if divisor == 0 || divisor > 0xFFFF {
            return Err(ConfigError::InvalidBaudrate);
        }

        let lcr: LineControlFlags = self.read_flags(UART_LCR);
        self.write_flags(UART_LCR, lcr | LineControlFlags::DIVISOR_LATCH_ACCESS);

        self.write_reg(UART_DLL, (divisor & 0xFF) as u8);
        self.write_reg(UART_DLH, ((divisor >> 8) & 0xFF) as u8);

        self.write_flags(UART_LCR, lcr);

        Ok(())
    }

    fn baudrate(&self, clock_freq: u32) -> u32 {
        let lcr: LineControlFlags = self.read_flags(UART_LCR);
        self.write_flags(UART_LCR, lcr | LineControlFlags::DIVISOR_LATCH_ACCESS);

        let dll = self.read_reg(UART_DLL) as u16;
        let dlh = self.read_reg(UART_DLH) as u16;

        self.write_flags(UART_LCR, lcr);

        let divisor = dll | (dlh << 8);

        if divisor == 0 {
            return 0;
        }

        clock_freq / (16 * divisor as u32)
    }

    fn init(&self) {
        self.write_flags(UART_IER, InterruptEnableFlags::empty());
        self.write_flags(
            UART_FCR,
            FifoControlFlags::ENABLE_FIFO
                | FifoControlFlags::CLEAR_RECEIVER_FIFO
                | FifoControlFlags::CLEAR_TRANSMITTER_FIFO
                | FifoControlFlags::TRIGGER_1_BYTE,
        );

        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.insert(
            ModemControlFlags::DATA_TERMINAL_READY
                | ModemControlFlags::REQUEST_TO_SEND
                | ModemControlFlags::OUT_2,
        );
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

/// IRQ endpoint for an NS16550-compatible UART.
pub struct Ns16550Irq<T: Kind> {
    base: T,
    saved_lsr: LineStatusFlags,
}

/// Restricted non-blocking TX view used only for emergency output.
pub struct Ns16550EmergencyTx<T: Kind> {
    base: T,
}

impl<T: Kind> Ns16550EmergencyTx<T> {
    fn mask_interrupts(&self) {
        self.base
            .write_flags(UART_IER, InterruptEnableFlags::empty());
        // Flush a posted MMIO write before the emergency path touches TX.
        let _: InterruptEnableFlags = self.base.read_flags(UART_IER);
    }
}

impl<T: Kind> UartEmergencyTx for Ns16550EmergencyTx<T> {
    unsafe fn mask_interrupts_unlocked(&self) {
        self.mask_interrupts();
    }

    unsafe fn try_write_unlocked(&self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes.iter().take(UART_FIFO_SIZE as usize) {
            let status: LineStatusFlags = self.base.read_flags(UART_LSR);
            if !status.contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY) {
                break;
            }
            self.base.write_reg(UART_THR, byte);
            written += 1;
        }
        written
    }
}

impl<T: Kind> Ns16550Irq<T> {
    fn next_event(&self) -> Option<SerialEventSet> {
        let iir: InterruptIdentificationFlags = self.base.read_flags(UART_IIR);
        if iir.bits() & (UART_IIR_ID | UART_IIR_NO_INT) == UART_IIR_BUSY {
            return Some(SerialEventSet::BUSY_DETECT);
        }
        if iir.contains(InterruptIdentificationFlags::NO_INTERRUPT_PENDING) {
            return None;
        }

        let interrupt_id = iir & InterruptIdentificationFlags::INTERRUPT_ID_MASK;
        let event = if interrupt_id == InterruptIdentificationFlags::RECEIVER_LINE_STATUS {
            SerialEventSet::RX_STATUS
        } else if interrupt_id == InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE {
            SerialEventSet::RX_DATA
        } else if interrupt_id == InterruptIdentificationFlags::CHARACTER_TIMEOUT {
            SerialEventSet::RX_TIMEOUT
        } else if interrupt_id == InterruptIdentificationFlags::TRANSMITTER_HOLDING_EMPTY {
            SerialEventSet::TX_SPACE
        } else if interrupt_id == InterruptIdentificationFlags::MODEM_STATUS {
            SerialEventSet::MODEM_STATUS
        } else {
            SerialEventSet::FAULT
        };
        Some(event)
    }

    fn ack_modem_status(&self) {
        let _: ModemStatusFlags = self.base.read_flags(UART_MSR);
    }

    fn ack_busy_detect(&self) {
        self.base.ack_busy_detect();
    }

    fn mask_sources(&self, events: SerialEventSet) {
        let mut ier: InterruptEnableFlags = self.base.read_flags(UART_IER);
        ier.remove(interrupt_enable_for_events(events));
        self.base.write_flags(UART_IER, ier);
    }
}

impl<T: Kind> UartIrq for Ns16550Irq<T> {
    fn mask(&mut self, sources: SerialEventSet) {
        self.mask_sources(sources);
    }

    fn handle(&mut self) -> Option<SerialIrqReport> {
        const IRQ_PASS_BUDGET: usize = 32;

        let mut event = SerialIrqEvent::default();
        let mut rx = IrqRxBatch::new();
        let mut rx_samples = 0;
        let mut pass_budget_exhausted = false;
        for pass in 0..IRQ_PASS_BUDGET {
            let Some(current) = self.next_event() else {
                break;
            };
            pass_budget_exhausted = pass + 1 == IRQ_PASS_BUDGET;
            event.events |= current;
            if current.intersects(SerialEventSet::RX) {
                let before = rx_samples;
                while rx_samples < IRQ_RX_BATCH_CAPACITY {
                    let Some(sample) = read_rx_sample(&self.base, &mut self.saved_lsr) else {
                        break;
                    };
                    event.rx_errors |= rx_errors_from_sample(sample);
                    rx.try_push(sample)
                        .expect("the fixed NS16550 IRQ loop cannot overflow its RX batch");
                    rx_samples += 1;
                }
                if rx_samples == IRQ_RX_BATCH_CAPACITY || rx_samples == before {
                    break;
                }
            }
            if current.contains(SerialEventSet::MODEM_STATUS) {
                self.ack_modem_status();
            }
            if current.contains(SerialEventSet::BUSY_DETECT) {
                self.ack_busy_detect();
            }
            if current.contains(SerialEventSet::FAULT) {
                self.base
                    .write_flags(UART_IER, InterruptEnableFlags::empty());
                break;
            }

            let rearm = current & SerialEventSet::TX_SPACE;
            if !rearm.is_empty() {
                self.mask_sources(rearm);
                event.rearm |= rearm;
            }
        }

        let defer_rx = rx.len() == IRQ_RX_BATCH_CAPACITY
            || event.rx_errors.contains(RxErrorFlags::OVERRUN)
            || (pass_budget_exhausted && event.events.has_rx());
        if defer_rx && !event.events.contains(SerialEventSet::FAULT) {
            self.mask_sources(SerialEventSet::RX);
            event.rearm |= SerialEventSet::RX;
        }

        (!event.events.is_empty()).then_some(SerialIrqReport::new(event, rx))
    }
}

impl<T: Kind> UartPort for Ns16550<T> {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        let original_ier: InterruptEnableFlags = self.read_flags(UART_IER);
        self.write_flags(UART_IER, InterruptEnableFlags::empty());
        if let Err(error) = self.set_config(config) {
            // Every current `Kind::set_baudrate` validates before its first
            // register write, while the remaining typed settings are
            // infallible. Restore the only register changed before config.
            self.write_flags(UART_IER, original_ier);
            return Err(error);
        }
        self.enable_fifo(true);

        let mut mcr: ModemControlFlags = self.read_flags(UART_MCR);
        mcr.insert(
            ModemControlFlags::DATA_TERMINAL_READY
                | ModemControlFlags::REQUEST_TO_SEND
                | ModemControlFlags::OUT_2,
        );
        self.write_flags(UART_MCR, mcr);
        self.saved_lsr = LineStatusFlags::empty();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.close();
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

    fn read_rx(&mut self) -> Option<RxSample> {
        Ns16550::read_rx(self)
    }

    fn discard_rx(&mut self) {
        self.saved_lsr = LineStatusFlags::empty();
        self.write_flags(
            UART_FCR,
            FifoControlFlags::ENABLE_FIFO
                | FifoControlFlags::CLEAR_RECEIVER_FIFO
                | FifoControlFlags::TRIGGER_8_BYTES,
        );
    }

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        self.try_write(bytes)
    }

    fn discard_tx(&mut self) -> bool {
        self.write_flags(
            UART_FCR,
            FifoControlFlags::ENABLE_FIFO
                | FifoControlFlags::CLEAR_TRANSMITTER_FIFO
                | FifoControlFlags::TRIGGER_8_BYTES,
        );
        true
    }

    fn tx_idle(&mut self) -> bool {
        let lsr: LineStatusFlags = self.read_flags(UART_LSR);
        lsr.contains(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY | LineStatusFlags::TRANSMITTER_EMPTY,
        )
    }

    fn mask(&mut self, sources: SerialEventSet) {
        let mut ier: InterruptEnableFlags = self.read_flags(UART_IER);
        ier.remove(interrupt_enable_for_events(sources));
        self.write_flags(UART_IER, ier);
    }

    fn mask_all(&mut self) {
        self.write_flags(UART_IER, InterruptEnableFlags::empty());
    }

    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet {
        let mut ier: InterruptEnableFlags = self.read_flags(UART_IER);
        ier.insert(interrupt_enable_for_events(sources));
        self.write_flags(UART_IER, ier);

        let lsr = self.read_lsr_preserving();
        let mut ready = SerialEventSet::empty();
        if sources.intersects(SerialEventSet::RX)
            && lsr.intersects(LineStatusFlags::DATA_READY | LineStatusFlags::ERROR_MASK)
        {
            ready |= if lsr.contains(LineStatusFlags::DATA_READY) {
                SerialEventSet::RX_DATA
            } else {
                SerialEventSet::RX_STATUS
            };
        }
        if sources.contains(SerialEventSet::TX_SPACE)
            && lsr.contains(LineStatusFlags::TRANSMITTER_HOLDING_EMPTY)
        {
            ready |= SerialEventSet::TX_SPACE;
        }
        if !ready.is_empty() {
            ier.remove(interrupt_enable_for_events(ready));
            self.write_flags(UART_IER, ier);
        }
        ready
    }
}

impl<T: Kind> SplitUart for Ns16550<T> {
    type Control = Self;
    type Irq = Ns16550Irq<T>;
    type EmergencyTx = Ns16550EmergencyTx<T>;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "NS16550 UART",
            register_base: self.base.get_base(),
            initial_baudrate: self.base.baudrate(self.clock_freq),
        }
    }

    fn split(self) -> SerialParts<Self::Control, Self::Irq, Self::EmergencyTx> {
        let irq = Ns16550Irq {
            base: self.base.clone(),
            saved_lsr: LineStatusFlags::empty(),
        };
        let emergency_tx = Ns16550EmergencyTx {
            base: self.base.clone(),
        };
        SerialParts::new(self, irq, emergency_tx)
    }
}

impl<T: Kind> PollingUart for Ns16550<T> {
    fn poll_status(&mut self) -> SerialEvent {
        Ns16550::poll_status(self)
    }

    fn write_byte(&mut self, byte: u8) {
        Ns16550::write_byte(self, byte);
    }

    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        Ns16550::read_byte(self, status)
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

    pub fn poll_status(&mut self) -> SerialEvent {
        serial_event_from_lsr(self.read_lsr_preserving())
    }

    pub fn try_write(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        while written < bytes.len() {
            let status = self.poll_status();
            if !status.tx_ready() {
                break;
            }
            self.write_byte(bytes[written]);
            written += 1;
        }
        written
    }

    pub fn try_read(&mut self, bytes: &mut [u8]) -> Result<usize, TransBytesError> {
        let mut read_count = 0;
        let mut first_error = None;
        for byte in bytes.iter_mut() {
            let status = self.poll_status();
            if !status.rx_ready() && !status.rx_error() {
                break;
            }
            let result = self.read_byte(status);
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

    pub fn write_byte(&mut self, byte: u8) {
        self.base.write_reg(UART_THR, byte);
    }

    pub fn read_rx(&mut self) -> Option<RxSample> {
        read_rx_sample(&self.base, &mut self.saved_lsr)
    }

    fn read_lsr_preserving(&mut self) -> LineStatusFlags {
        let lsr: LineStatusFlags = self.read_flags(UART_LSR);
        self.saved_lsr
            .insert(lsr & (LineStatusFlags::ERROR_MASK | LineStatusFlags::FIFO_ERROR));
        lsr | self.saved_lsr
    }

    pub fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        if !status.rx_ready() && !status.rx_error() {
            return None;
        }
        if self.saved_lsr.contains(LineStatusFlags::OVERRUN_ERROR) {
            let b = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::OVERRUN_ERROR);
            return Some(Err(TransferError::Overrun(b)));
        }
        if self.saved_lsr.contains(LineStatusFlags::PARITY_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::PARITY_ERROR);
            return Some(Err(TransferError::Parity));
        }
        if self.saved_lsr.contains(LineStatusFlags::FRAMING_ERROR) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::FRAMING_ERROR);
            return Some(Err(TransferError::Framing));
        }
        if self.saved_lsr.contains(LineStatusFlags::BREAK_INTERRUPT) {
            let _ = self.base.read_reg(UART_RBR);
            self.saved_lsr.remove(LineStatusFlags::BREAK_INTERRUPT);
            return Some(Err(TransferError::Break));
        }
        if status.rx_ready() {
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

    pub fn set_irq_mask(&mut self, events: SerialEventSet) {
        self.write_flags(UART_IER, interrupt_enable_for_events(events));
    }

    pub fn get_irq_mask(&self) -> SerialEventSet {
        let ier: InterruptEnableFlags = self.read_flags(UART_IER);
        let mut events = SerialEventSet::empty();

        if ier.contains(InterruptEnableFlags::RECEIVED_DATA_AVAILABLE) {
            events |= SerialEventSet::RX_DATA;
        }
        if ier.contains(InterruptEnableFlags::RECEIVER_LINE_STATUS) {
            events |= SerialEventSet::RX_STATUS;
        }
        if ier.contains(InterruptEnableFlags::TRANSMITTER_HOLDING_EMPTY) {
            events |= SerialEventSet::TX_SPACE;
        }

        events
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
        if enable {
            let mut fcr = FifoControlFlags::ENABLE_FIFO;
            fcr.insert(FifoControlFlags::CLEAR_RECEIVER_FIFO);
            fcr.insert(FifoControlFlags::CLEAR_TRANSMITTER_FIFO);
            // Match Linux's 16550A default. A half-full threshold leaves FIFO
            // headroom for deferred service while avoiding one IRQ wakeup per
            // character on high-baudrate DesignWare UARTs.
            fcr.insert(FifoControlFlags::TRIGGER_8_BYTES);
            self.write_flags(UART_FCR, fcr);
            if self.is_fifo_enabled() {
                return;
            }
        }
        self.write_flags(UART_FCR, FifoControlFlags::empty());
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
}

fn read_rx_sample<T: Kind>(base: &T, saved_lsr: &mut LineStatusFlags) -> Option<RxSample> {
    let current: LineStatusFlags = base.read_flags(UART_LSR);
    saved_lsr.insert(current & (LineStatusFlags::ERROR_MASK | LineStatusFlags::FIFO_ERROR));
    let lsr = current | *saved_lsr;
    if !lsr.intersects(LineStatusFlags::DATA_READY | LineStatusFlags::ERROR_MASK) {
        return None;
    }

    let byte = lsr
        .contains(LineStatusFlags::DATA_READY)
        .then(|| base.read_reg(UART_RBR));
    let flag = if lsr.contains(LineStatusFlags::BREAK_INTERRUPT) {
        RxFlag::Break
    } else if lsr.contains(LineStatusFlags::PARITY_ERROR) {
        RxFlag::Parity
    } else if lsr.contains(LineStatusFlags::FRAMING_ERROR) {
        RxFlag::Framing
    } else {
        RxFlag::Normal
    };
    let overrun = lsr.contains(LineStatusFlags::OVERRUN_ERROR);
    saved_lsr.remove(LineStatusFlags::ERROR_MASK | LineStatusFlags::FIFO_ERROR);

    Some(RxSample {
        byte,
        flag,
        overrun,
    })
}

fn rx_errors_from_sample(sample: RxSample) -> RxErrorFlags {
    let mut errors = match sample.flag {
        RxFlag::Normal => RxErrorFlags::empty(),
        RxFlag::Break => RxErrorFlags::BREAK,
        RxFlag::Parity => RxErrorFlags::PARITY,
        RxFlag::Framing => RxErrorFlags::FRAMING,
    };
    if sample.overrun {
        errors |= RxErrorFlags::OVERRUN;
    }
    errors
}

fn interrupt_enable_for_events(events: SerialEventSet) -> InterruptEnableFlags {
    let mut ier = InterruptEnableFlags::empty();
    if events.intersects(SerialEventSet::RX) {
        ier.insert(
            InterruptEnableFlags::RECEIVED_DATA_AVAILABLE
                | InterruptEnableFlags::RECEIVER_LINE_STATUS,
        );
    }
    if events.contains(SerialEventSet::TX_SPACE) {
        ier.insert(InterruptEnableFlags::TRANSMITTER_HOLDING_EMPTY);
    }
    ier
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
