//! Guest-visible PL011 register state and receive FIFO transitions.

use super::{RxErrors, RxResult, registers::*};

#[derive(Clone, Copy, Debug, Default)]
struct RxWord {
    byte: u8,
    errors: RxErrors,
}

struct RxFifo {
    words: [RxWord; FIFO_CAPACITY],
    head: usize,
    count: usize,
}

impl Default for RxFifo {
    fn default() -> Self {
        Self {
            words: [RxWord::default(); FIFO_CAPACITY],
            head: 0,
            count: 0,
        }
    }
}

impl RxFifo {
    const fn is_empty(&self) -> bool {
        self.count == 0
    }

    const fn len(&self) -> usize {
        self.count
    }

    fn push(&mut self, word: RxWord, capacity: usize) -> bool {
        if self.count >= capacity {
            return false;
        }
        let tail = (self.head + self.count) % FIFO_CAPACITY;
        self.words[tail] = word;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<RxWord> {
        if self.count == 0 {
            return None;
        }
        let word = self.words[self.head];
        self.head = (self.head + 1) % FIFO_CAPACITY;
        self.count -= 1;
        Some(word)
    }

    fn truncate_to_one(&mut self) {
        if self.count > 1 {
            self.count = 1;
        }
    }
}

pub(crate) struct Pl011State {
    rx: RxFifo,
    error_status: RxErrors,
    ilpr: u32,
    integer_baud: u32,
    fractional_baud: u32,
    line_control: u32,
    control: u32,
    fifo_levels: u32,
    interrupt_mask: u32,
    receive_timeout: bool,
    timeout_armed: bool,
    dma_control: u32,
    irq_asserted: bool,
    generation: u64,
}

impl Default for Pl011State {
    fn default() -> Self {
        Self {
            rx: RxFifo::default(),
            error_status: RxErrors::empty(),
            ilpr: 0,
            integer_baud: 0,
            fractional_baud: 0,
            line_control: 0,
            control: UARTCR_TXE | UARTCR_RXE,
            fifo_levels: 0x12,
            interrupt_mask: 0,
            receive_timeout: false,
            timeout_armed: false,
            dma_control: 0,
            irq_asserted: false,
            generation: 0,
        }
    }
}

impl Pl011State {
    pub(crate) fn receive_ready(&self) -> bool {
        self.control & (UARTCR_UARTEN | UARTCR_RXE) == (UARTCR_UARTEN | UARTCR_RXE)
            && self.rx.len() < self.rx_capacity()
    }

    pub(crate) fn receive(&mut self, byte: u8, errors: RxErrors) -> RxResult {
        if self.control & (UARTCR_UARTEN | UARTCR_RXE) != (UARTCR_UARTEN | UARTCR_RXE) {
            return RxResult::ReceiverDisabled;
        }
        self.receive_timeout = false;
        self.timeout_armed = true;
        self.error_status |= errors;
        if !self.rx.push(RxWord { byte, errors }, self.rx_capacity()) {
            self.error_status |= RxErrors::OVERRUN;
            return RxResult::DroppedOverrun;
        }
        RxResult::Accepted
    }

    pub(crate) fn expire_receive_timeout(&mut self) {
        if self.timeout_armed && !self.rx.is_empty() {
            self.receive_timeout = true;
            self.timeout_armed = false;
            self.changed();
        }
    }

    pub(crate) fn read(&mut self, offset: u64) -> u32 {
        match offset {
            UARTDR => self.read_data(),
            UARTRSR_ECR => u32::from(self.error_status.bits()),
            UARTFR => self.flags(),
            UARTILPR => self.ilpr,
            UARTIBRD => self.integer_baud,
            UARTFBRD => self.fractional_baud,
            UARTLCR_H => self.line_control,
            UARTCR => self.control,
            UARTIFLS => self.fifo_levels,
            UARTIMSC => self.interrupt_mask,
            UARTRIS => self.raw_interrupts(),
            UARTMIS => self.raw_interrupts() & self.interrupt_mask,
            UARTDMACR => self.dma_control,
            UART_PID0 => 0x11,
            UART_PID1 => 0x10,
            UART_PID2 => 0x14,
            UART_PID3 => 0x00,
            UART_CID0 => 0x0d,
            UART_CID1 => 0xf0,
            UART_CID2 => 0x05,
            UART_CID3 => 0xb1,
            _ => 0,
        }
    }

    pub(crate) fn write(&mut self, offset: u64, value: u32) -> Option<u8> {
        if offset == UARTDR {
            return self.transmit(value as u8);
        }
        match offset {
            UARTRSR_ECR => self.error_status = RxErrors::empty(),
            UARTILPR => self.ilpr = value & 0xff,
            UARTIBRD => self.integer_baud = value & 0xffff,
            UARTFBRD => self.fractional_baud = value & 0x3f,
            UARTLCR_H => {
                let fifo_was_enabled = self.fifo_enabled();
                self.line_control = value & 0xff;
                if fifo_was_enabled && !self.fifo_enabled() && self.rx.len() > 1 {
                    self.rx.truncate_to_one();
                    self.error_status |= RxErrors::OVERRUN;
                }
            }
            UARTCR => self.control = value & 0xffff,
            UARTIFLS => self.fifo_levels = value & 0x3f,
            UARTIMSC => self.interrupt_mask = value & UARTINT_ALL,
            UARTICR => self.clear_interrupts(value),
            UARTDMACR => self.dma_control = value & 0x7,
            _ => {}
        }
        None
    }

    pub(crate) fn irq_snapshot(&self) -> (bool, bool, u64) {
        (self.interrupt_pending(), self.irq_asserted, self.generation)
    }

    pub(crate) fn record_irq_level(&mut self, asserted: bool, generation: u64) -> bool {
        self.irq_asserted = asserted;
        self.generation == generation && self.interrupt_pending() == asserted
    }

    pub(crate) fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn read_data(&mut self) -> u32 {
        let Some(word) = self.rx.pop() else {
            return 0;
        };
        self.receive_timeout = false;
        self.timeout_armed = !self.rx.is_empty();
        u32::from(word.byte) | (u32::from(word.errors.bits()) << 8)
    }

    fn transmit(&self, byte: u8) -> Option<u8> {
        (self.control & (UARTCR_UARTEN | UARTCR_TXE) == (UARTCR_UARTEN | UARTCR_TXE))
            .then_some(byte)
    }

    fn flags(&self) -> u32 {
        let mut flags = UARTFR_TXFE;
        if self.rx.is_empty() {
            flags |= UARTFR_RXFE;
        }
        if self.rx.len() >= self.rx_capacity() {
            flags |= UARTFR_RXFF;
        }
        flags & !(UARTFR_BUSY | UARTFR_TXFF)
    }

    fn raw_interrupts(&self) -> u32 {
        if self.control & UARTCR_UARTEN == 0 {
            return 0;
        }
        let mut raw = 0;
        if self.control & UARTCR_RXE != 0 && self.rx.len() >= self.rx_threshold() {
            raw |= UARTINT_RX;
        }
        if self.control & UARTCR_TXE != 0 {
            raw |= UARTINT_TX;
        }
        if self.receive_timeout {
            raw |= UARTINT_RT;
        }
        if self.error_status.contains(RxErrors::FRAMING) {
            raw |= UARTINT_FE;
        }
        if self.error_status.contains(RxErrors::PARITY) {
            raw |= UARTINT_PE;
        }
        if self.error_status.contains(RxErrors::BREAK) {
            raw |= UARTINT_BE;
        }
        if self.error_status.contains(RxErrors::OVERRUN) {
            raw |= UARTINT_OE;
        }
        raw
    }

    fn clear_interrupts(&mut self, clear: u32) {
        if clear & UARTINT_RT != 0 {
            self.receive_timeout = false;
        }
        if clear & UARTINT_FE != 0 {
            self.error_status.remove(RxErrors::FRAMING);
        }
        if clear & UARTINT_PE != 0 {
            self.error_status.remove(RxErrors::PARITY);
        }
        if clear & UARTINT_BE != 0 {
            self.error_status.remove(RxErrors::BREAK);
        }
        if clear & UARTINT_OE != 0 {
            self.error_status.remove(RxErrors::OVERRUN);
        }
    }

    const fn fifo_enabled(&self) -> bool {
        self.line_control & UARTLCR_H_FEN != 0
    }

    const fn rx_capacity(&self) -> usize {
        if self.fifo_enabled() {
            FIFO_CAPACITY
        } else {
            1
        }
    }

    fn rx_threshold(&self) -> usize {
        if !self.fifo_enabled() {
            return 1;
        }
        match (self.fifo_levels >> 3) & 0x7 {
            0 => 2,
            1 => 4,
            2 => 8,
            3 => 12,
            4 => 14,
            _ => 14,
        }
    }

    fn interrupt_pending(&self) -> bool {
        self.raw_interrupts() & self.interrupt_mask != 0
    }
}
