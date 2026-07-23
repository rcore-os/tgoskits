//! NS16550/16450 UART 驱动模块
//!
//! 提供两种访问方式：
//! - IO Port 版本（x86_64 架构）
//! - MMIO 版本（通用嵌入式平台）

// 公共寄存器定义
mod registers;

use bitflags::Flags;
use rdif_serial::{
    Config, ConfigError, DataBits, IrqRxSink, Parity, RxErrorFlags, RxFlag, RxSample,
    SerialEventSet, SerialIrqEvent, SplitUart, StopBits, UartInfo, UartIrq, UartParts, UartPort,
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

    fn mask(&self, events: SerialEventSet) {
        let mut ier: InterruptEnableFlags = self.base.read_flags(UART_IER);
        ier.remove(interrupt_enable_for_events(events));
        self.base.write_flags(UART_IER, ier);
    }
}

impl<T: Kind> UartIrq for Ns16550Irq<T> {
    fn handle(&mut self, rx: &mut dyn IrqRxSink) -> Option<SerialIrqEvent> {
        const IRQ_PASS_BUDGET: usize = 32;
        const RX_SAMPLE_BUDGET: usize = 256;

        let mut event = SerialIrqEvent::default();
        let mut rx_samples = 0;
        for _ in 0..IRQ_PASS_BUDGET {
            let Some(current) = self.next_event() else {
                break;
            };
            event.events |= current;
            if current.intersects(SerialEventSet::RX) {
                let before = rx_samples;
                while rx_samples < RX_SAMPLE_BUDGET {
                    let Some(sample) = read_rx_sample(&self.base, &mut self.saved_lsr) else {
                        break;
                    };
                    event.rx_errors |= rx_errors_from_sample(sample);
                    rx.push(sample);
                    rx_samples += 1;
                }
                if rx_samples == RX_SAMPLE_BUDGET || rx_samples == before {
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
                self.mask(rearm);
                event.rearm |= rearm;
            }
        }

        (!event.events.is_empty()).then_some(event)
    }
}

impl<T: Kind> UartPort for Ns16550<T> {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.write_flags(UART_IER, InterruptEnableFlags::empty());
        self.set_config(config)?;
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

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        self.try_write(bytes)
    }

    fn tx_idle(&mut self) -> bool {
        let lsr: LineStatusFlags = self.read_flags(UART_LSR);
        lsr.contains(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY | LineStatusFlags::TRANSMITTER_EMPTY,
        )
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
    type Port = Self;
    type Irq = Ns16550Irq<T>;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "NS16550 UART",
            register_base: self.base.get_base(),
            initial_baudrate: self.base.baudrate(self.clock_freq),
        }
    }

    fn split(self) -> UartParts<Self::Port, Self::Irq> {
        let irq = Ns16550Irq {
            base: self.base.clone(),
            saved_lsr: LineStatusFlags::empty(),
        };
        UartParts::new(self, irq)
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
    use std::{
        sync::{Arc, Mutex, MutexGuard},
        vec::Vec,
    };

    use super::*;

    static REGS: [AtomicU8; 8] = [const { AtomicU8::new(0) }; 8];
    static DLL_REG: AtomicU8 = AtomicU8::new(0);
    static DLH_REG: AtomicU8 = AtomicU8::new(0);
    static THR_WRITES: AtomicUsize = AtomicUsize::new(0);
    static RBR_READS: AtomicUsize = AtomicUsize::new(0);
    static LSR_READS: AtomicUsize = AtomicUsize::new(0);
    static LAST_FCR_WRITE: AtomicU8 = AtomicU8::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Default)]
    struct CollectRx(Vec<RxSample>);

    impl IrqRxSink for CollectRx {
        fn push(&mut self, sample: RxSample) {
            self.0.push(sample);
        }
    }

    fn handle_irq(irq: &mut impl UartIrq) -> (Option<SerialIrqEvent>, Vec<RxSample>) {
        let mut rx = CollectRx::default();
        let event = irq.handle(&mut rx);
        (event, rx.0)
    }

    #[derive(Clone)]
    struct MockKind;

    impl Kind for MockKind {
        fn read_reg(&self, reg: u8) -> u8 {
            let dlab = REGS[UART_LCR as usize].load(Ordering::SeqCst)
                & LineControlFlags::DIVISOR_LATCH_ACCESS.bits()
                != 0;
            if dlab {
                return match reg {
                    UART_DLL => DLL_REG.load(Ordering::SeqCst),
                    UART_DLH => DLH_REG.load(Ordering::SeqCst),
                    _ => REGS[reg as usize].load(Ordering::SeqCst),
                };
            }

            let value = REGS[reg as usize].load(Ordering::SeqCst);
            if reg == UART_LSR {
                LSR_READS.fetch_add(1, Ordering::SeqCst);
            }
            if reg == UART_RBR {
                RBR_READS.fetch_add(1, Ordering::SeqCst);
                REGS[UART_LSR as usize].fetch_and(
                    !(LineStatusFlags::ERROR_MASK | LineStatusFlags::DATA_READY).bits(),
                    Ordering::SeqCst,
                );
            } else if reg == UART_MSR {
                REGS[UART_MSR as usize]
                    .fetch_and(!ModemStatusFlags::DELTA_MASK.bits(), Ordering::SeqCst);
            }
            value
        }

        fn write_reg(&self, reg: u8, val: u8) {
            let dlab = REGS[UART_LCR as usize].load(Ordering::SeqCst)
                & LineControlFlags::DIVISOR_LATCH_ACCESS.bits()
                != 0;
            if dlab {
                match reg {
                    UART_DLL => {
                        DLL_REG.store(val, Ordering::SeqCst);
                        return;
                    }
                    UART_DLH => {
                        DLH_REG.store(val, Ordering::SeqCst);
                        return;
                    }
                    _ => {}
                }
            }

            REGS[reg as usize].store(val, Ordering::SeqCst);
            if reg == UART_FCR {
                LAST_FCR_WRITE.store(val, Ordering::SeqCst);
                if val & FifoControlFlags::ENABLE_FIFO.bits() != 0 {
                    REGS[UART_IIR as usize].fetch_or(
                        InterruptIdentificationFlags::FIFO_ENABLE_MASK.bits(),
                        Ordering::SeqCst,
                    );
                } else {
                    REGS[UART_IIR as usize].fetch_and(
                        !InterruptIdentificationFlags::FIFO_ENABLE_MASK.bits(),
                        Ordering::SeqCst,
                    );
                }
            }
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

    #[derive(Clone)]
    struct FloodKind {
        rbr_reads: Arc<AtomicUsize>,
    }

    impl Kind for FloodKind {
        fn read_reg(&self, reg: u8) -> u8 {
            match reg {
                UART_IIR => InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE.bits(),
                UART_LSR => LineStatusFlags::DATA_READY.bits(),
                UART_RBR => self.rbr_reads.fetch_add(1, Ordering::SeqCst) as u8,
                _ => 0,
            }
        }

        fn write_reg(&self, _reg: u8, _val: u8) {}

        fn get_base(&self) -> usize {
            0x2000
        }
    }

    fn reset_regs() {
        for reg in &REGS {
            reg.store(0, Ordering::SeqCst);
        }
        DLL_REG.store(0, Ordering::SeqCst);
        DLH_REG.store(0, Ordering::SeqCst);
        THR_WRITES.store(0, Ordering::SeqCst);
        RBR_READS.store(0, Ordering::SeqCst);
        LSR_READS.store(0, Ordering::SeqCst);
        LAST_FCR_WRITE.store(0, Ordering::SeqCst);
    }

    fn serial() -> (MutexGuard<'static, ()>, Ns16550<MockKind>) {
        let guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
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

    fn started_parts(
        uart: Ns16550<MockKind>,
    ) -> UartParts<Ns16550<MockKind>, Ns16550Irq<MockKind>> {
        let mut parts = uart.split();
        parts.port.startup(&Config::new()).unwrap();
        parts
    }

    #[test]
    fn baudrate_reads_divisor_latch_without_consuming_rx_register() {
        let (_guard, uart) = serial();
        let original_lcr = LineControlFlags::WORD_LENGTH_8 | LineControlFlags::STOP_BITS;
        REGS[UART_LCR as usize].store(original_lcr.bits(), Ordering::SeqCst);
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        REGS[UART_RBR as usize].store(0, Ordering::SeqCst);
        REGS[UART_IER as usize].store(0, Ordering::SeqCst);
        DLL_REG.store(1, Ordering::SeqCst);
        DLH_REG.store(0, Ordering::SeqCst);

        assert_eq!(uart.runtime_info().initial_baudrate, 115_200);
        assert_eq!(
            REGS[UART_LCR as usize].load(Ordering::SeqCst),
            original_lcr.bits()
        );
        assert!(
            LineStatusFlags::from_bits_retain(REGS[UART_LSR as usize].load(Ordering::SeqCst))
                .contains(LineStatusFlags::DATA_READY)
        );
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
    fn open_enables_modem_interrupt_output_gate() {
        let (_guard, mut uart) = serial();

        uart.open();

        let fcr = FifoControlFlags::from_bits_retain(LAST_FCR_WRITE.load(Ordering::SeqCst));
        assert!(fcr.contains(FifoControlFlags::ENABLE_FIFO));
        assert!(fcr.contains(FifoControlFlags::CLEAR_RECEIVER_FIFO));
        assert!(fcr.contains(FifoControlFlags::CLEAR_TRANSMITTER_FIFO));
        let mcr =
            ModemControlFlags::from_bits_retain(REGS[UART_MCR as usize].load(Ordering::SeqCst));
        assert!(mcr.contains(ModemControlFlags::DATA_TERMINAL_READY));
        assert!(mcr.contains(ModemControlFlags::REQUEST_TO_SEND));
        assert!(mcr.contains(ModemControlFlags::OUT_2));
    }

    #[test]
    fn startup_enables_fifo_before_checking_fifo_status() {
        let (_guard, mut uart) = serial();

        uart.startup(&Config::new()).unwrap();

        let iir = InterruptIdentificationFlags::from_bits_retain(
            REGS[UART_IIR as usize].load(Ordering::SeqCst),
        );
        assert!(iir.contains(InterruptIdentificationFlags::FIFO_ENABLE_MASK));
        assert_eq!(THR_WRITES.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn startup_uses_half_full_rx_trigger_for_deferred_service() {
        let (_guard, mut uart) = serial();

        uart.startup(&Config::new()).unwrap();

        let fcr = FifoControlFlags::from_bits_retain(LAST_FCR_WRITE.load(Ordering::SeqCst));
        assert_eq!(
            fcr & FifoControlFlags::TRIGGER_LEVEL_MASK,
            FifoControlFlags::TRIGGER_8_BYTES,
            "deferred RX service must amortize IRQ wakeups at the Linux 16550A default trigger",
        );
    }

    #[test]
    fn try_read_empty_returns_zero() {
        let (_guard, mut uart) = serial();
        let mut buf = [0];

        assert_eq!(uart.try_read(&mut buf), Ok(0));
    }

    #[test]
    fn irq_reports_rx_error_and_buffers_fifo_data() {
        let (_guard, uart) = serial();
        let mut parts = uart.split();
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::OVERRUN_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(0xab, Ordering::SeqCst);

        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();
        assert!(event.events.contains(SerialEventSet::RX_STATUS));
        assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
        assert_eq!(
            samples,
            [RxSample {
                byte: Some(0xab),
                flag: RxFlag::Normal,
                overrun: true,
            }]
        );
        assert_eq!(RBR_READS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn split_endpoints_service_rx_and_tx_fifo() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        let event = handle_irq(&mut parts.irq).0.unwrap();
        assert!(event.events.contains(SerialEventSet::TX_SPACE));
        assert_eq!(parts.port.write_tx(b"ab"), 1);
        assert_eq!(REGS[UART_THR as usize].load(Ordering::SeqCst), b'a');

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        REGS[UART_RBR as usize].store(b'z', Ordering::SeqCst);
        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();
        assert!(event.events.contains(SerialEventSet::RX_DATA));
        assert_eq!(
            samples,
            [RxSample {
                byte: Some(b'z'),
                flag: RxFlag::Normal,
                overrun: false,
            }]
        );
    }

    #[test]
    fn hard_irq_drains_rx_before_deferred_worker_can_overrun_fifo() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::PARITY_ERROR).bits(),
            Ordering::SeqCst,
        );
        LSR_READS.store(0, Ordering::SeqCst);

        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();

        assert!(event.events.contains(SerialEventSet::RX_STATUS));
        assert!(event.rx_errors.contains(RxErrorFlags::PARITY));
        assert!(LSR_READS.load(Ordering::SeqCst) > 0);
        assert_eq!(
            RBR_READS.load(Ordering::SeqCst),
            1,
            "the hard IRQ must free a bounded hardware FIFO slot before the worker runs",
        );
        assert_eq!(THR_WRITES.load(Ordering::SeqCst), 0);
        assert_eq!(REGS[UART_LSR as usize].load(Ordering::SeqCst), 0);
        assert_eq!(
            samples,
            [RxSample {
                byte: Some(0),
                flag: RxFlag::Parity,
                overrun: false,
            }]
        );
    }

    #[test]
    fn hard_irq_rx_drain_is_bounded_to_256_samples() {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut irq = Ns16550Irq {
            base: FloodKind {
                rbr_reads: reads.clone(),
            },
            saved_lsr: LineStatusFlags::empty(),
        };

        let (event, samples) = handle_irq(&mut irq);

        assert!(event.unwrap().events.contains(SerialEventSet::RX_DATA));
        assert_eq!(samples.len(), 256);
        assert_eq!(reads.load(Ordering::SeqCst), 256);
    }

    #[test]
    fn irq_endpoint_does_not_synthesize_tx_irq_from_plain_lsr_ready() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::NO_INTERRUPT_PENDING.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );

        assert!(handle_irq(&mut parts.irq).0.is_none());
    }

    #[test]
    fn hard_irq_does_not_claim_tx_ready_without_iir_pending() {
        let (_guard, uart) = serial();
        let mut parts = uart.split();
        parts.port.set_irq_mask(SerialEventSet::TX_SPACE);
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::NO_INTERRUPT_PENDING.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );

        assert!(handle_irq(&mut parts.irq).0.is_none());
        assert!(parts.port.poll_status().tx_ready());
    }

    #[test]
    fn hard_irq_does_not_claim_rx_ready_without_iir_pending() {
        let (_guard, uart) = serial();
        let mut parts = uart.split();
        parts.port.set_irq_mask(SerialEventSet::RX);
        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::NO_INTERRUPT_PENDING.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);

        assert!(handle_irq(&mut parts.irq).0.is_none());
        assert!(parts.port.poll_status().rx_ready());
    }

    #[test]
    fn hard_irq_claims_and_clears_modem_status_interrupt() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::MODEM_STATUS.bits()
                | InterruptIdentificationFlags::FIFO_ENABLE_MASK.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_MSR as usize].store(
            ModemStatusFlags::DELTA_CLEAR_TO_SEND.bits(),
            Ordering::SeqCst,
        );

        let event = handle_irq(&mut parts.irq).0.unwrap();
        assert!(event.events.contains(SerialEventSet::MODEM_STATUS));
        assert!(
            ModemStatusFlags::from_bits_retain(REGS[UART_MSR as usize].load(Ordering::SeqCst))
                .intersection(ModemStatusFlags::DELTA_MASK)
                .is_empty()
        );
    }

    #[test]
    fn irq_event_drains_rx_fifo_into_sink() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVED_DATA_AVAILABLE.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        REGS[UART_RBR as usize].store(b'r', Ordering::SeqCst);

        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();
        assert!(event.events.contains(SerialEventSet::RX_DATA));
        assert_eq!(
            samples,
            [RxSample {
                byte: Some(b'r'),
                flag: RxFlag::Normal,
                overrun: false,
            }]
        );
    }

    #[test]
    fn tx_irq_exposes_space_without_owning_a_software_fifo() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            LineStatusFlags::TRANSMITTER_HOLDING_EMPTY.bits(),
            Ordering::SeqCst,
        );

        let event = handle_irq(&mut parts.irq).0.unwrap();
        assert!(event.events.contains(SerialEventSet::TX_SPACE));
        assert_eq!(parts.port.write_tx(b"ab"), 1);
        assert_eq!(REGS[UART_THR as usize].load(Ordering::SeqCst), b'a');
    }

    #[test]
    fn irq_lsr_error_is_preserved_in_buffered_sample() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::PARITY_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(b'p', Ordering::SeqCst);

        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();
        assert!(event.rx_errors.contains(RxErrorFlags::PARITY));
        assert_eq!(
            samples,
            [RxSample {
                byte: Some(b'p'),
                flag: RxFlag::Parity,
                overrun: false,
            }]
        );
    }

    #[test]
    fn port_rx_returns_current_byte_and_overrun_marker() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);

        REGS[UART_IIR as usize].store(
            InterruptIdentificationFlags::RECEIVER_LINE_STATUS.bits(),
            Ordering::SeqCst,
        );
        REGS[UART_LSR as usize].store(
            (LineStatusFlags::DATA_READY | LineStatusFlags::OVERRUN_ERROR).bits(),
            Ordering::SeqCst,
        );
        REGS[UART_RBR as usize].store(b'S', Ordering::SeqCst);

        assert_eq!(
            parts.port.read_rx(),
            Some(RxSample {
                byte: Some(b'S'),
                flag: RxFlag::Normal,
                overrun: true,
            })
        );
    }

    #[test]
    fn irq_keeps_rx_source_enabled_after_draining_fifo() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);
        REGS[UART_IER as usize].store(UART_IER_RDI | UART_IER_RLSI, Ordering::SeqCst);
        REGS[UART_IIR as usize].store(UART_IIR_RDI, Ordering::SeqCst);
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);
        REGS[UART_RBR as usize].store(b'q', Ordering::SeqCst);

        let (event, samples) = handle_irq(&mut parts.irq);
        let event = event.unwrap();

        assert!(event.events.contains(SerialEventSet::RX_DATA));
        assert!(!event.rearm.intersects(SerialEventSet::RX));
        assert_eq!(
            REGS[UART_IER as usize].load(Ordering::SeqCst),
            UART_IER_RDI | UART_IER_RLSI
        );
        assert_eq!(RBR_READS.load(Ordering::SeqCst), 1);
        assert_eq!(samples[0].byte, Some(b'q'));
    }

    #[test]
    fn rearm_remasks_a_source_that_is_already_ready() {
        let (_guard, mut uart) = serial();
        uart.startup(&Config::new()).unwrap();
        REGS[UART_LSR as usize].store(LineStatusFlags::DATA_READY.bits(), Ordering::SeqCst);

        let ready = uart.rearm(SerialEventSet::RX);

        assert_eq!(ready, SerialEventSet::RX_DATA);
        assert_eq!(REGS[UART_IER as usize].load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unknown_irq_source_masks_all_and_reports_fault() {
        let (_guard, uart) = serial();
        let mut parts = started_parts(uart);
        REGS[UART_IER as usize].store(0xff, Ordering::SeqCst);
        REGS[UART_IIR as usize].store(0x08, Ordering::SeqCst);

        let event = handle_irq(&mut parts.irq).0.unwrap();

        assert!(event.events.contains(SerialEventSet::FAULT));
        assert_eq!(REGS[UART_IER as usize].load(Ordering::SeqCst), 0);
        assert_eq!(RBR_READS.load(Ordering::SeqCst), 0);
        assert_eq!(THR_WRITES.load(Ordering::SeqCst), 0);
    }
}
