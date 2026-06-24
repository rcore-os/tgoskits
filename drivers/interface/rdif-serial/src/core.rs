use crate::{
    Config, ConfigError, FixedQueue, InterruptMask, IrqSource, RawUart, RxFlag, RxItem,
    SerialCounters, SerialIrqOutcome,
};

pub const DEFAULT_TX_CAP: usize = 4096;
pub const DEFAULT_RX_CAP: usize = 4096;

pub const RX_IRQ_BUDGET: usize = 256;
pub const TX_IRQ_BUDGET: usize = 64;
pub const IRQ_PASS_BUDGET: usize = 32;
pub const TX_WAKEUP_WATERMARK: usize = 256;
pub const TX_KICK_BUDGET: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortState {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxEnqueue {
    pub accepted: usize,
    pub sent_immediately: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct TxService {
    sent: usize,
    wake_writers: bool,
}

/// Runtime UART core.
///
/// The core itself is lock-free and assumes the caller already holds the port
/// lock. It owns the raw register object, TX software FIFO, and RX flip FIFO.
/// IRQ code and task code must not access `raw` through any other path.
pub struct SerialCore<
    T: RawUart,
    const TX_CAP: usize = DEFAULT_TX_CAP,
    const RX_CAP: usize = DEFAULT_RX_CAP,
> {
    raw: T,
    tx_fifo: FixedQueue<u8, TX_CAP>,
    rx_fifo: FixedQueue<RxItem, RX_CAP>,
    irq_mask: InterruptMask,
    state: PortState,
    counters: SerialCounters,
}

impl<T: RawUart, const TX: usize, const RX: usize> SerialCore<T, TX, RX> {
    pub fn new(raw: T) -> Self {
        Self {
            raw,
            tx_fifo: FixedQueue::new(),
            rx_fifo: FixedQueue::new(),
            irq_mask: InterruptMask::empty(),
            state: PortState::Down,
            counters: SerialCounters::default(),
        }
    }

    pub fn raw(&self) -> &T {
        &self.raw
    }

    pub fn raw_mut(&mut self) -> &mut T {
        &mut self.raw
    }

    pub fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        if self.state == PortState::Up {
            return Ok(());
        }

        self.raw.startup(config)?;
        self.irq_mask = InterruptMask::RX;
        self.raw.set_irq_mask(self.irq_mask);
        self.state = PortState::Up;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        if self.state == PortState::Down {
            return;
        }

        self.set_irq_mask_locked(InterruptMask::empty());
        self.raw.shutdown();
        self.tx_fifo.clear();
        self.rx_fifo.clear();
        self.state = PortState::Down;
    }

    pub fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        let saved_mask = self.irq_mask;
        self.raw.set_irq_mask(InterruptMask::empty());
        let result = self.raw.set_config(config);
        self.raw.set_irq_mask(saved_mask);
        result
    }

    pub fn baudrate(&self) -> u32 {
        self.raw.baudrate()
    }

    pub fn write_room(&self) -> usize {
        self.tx_fifo.remaining()
    }

    pub fn chars_in_buffer(&self) -> usize {
        self.tx_fifo.len()
    }

    pub fn flush_tx_buffer(&mut self) {
        self.tx_fifo.clear();
        self.stop_tx_irq_locked();
    }

    pub fn tx_idle(&mut self) -> bool {
        self.tx_fifo.is_empty() && self.raw.tx_idle()
    }

    pub fn enqueue_tx(&mut self, bytes: &[u8]) -> TxEnqueue {
        if self.state != PortState::Up || bytes.is_empty() {
            return TxEnqueue::default();
        }

        let accepted = self.tx_fifo.push_slice(bytes);
        let service = if accepted > 0 {
            self.service_tx_locked(TX_KICK_BUDGET)
        } else {
            TxService::default()
        };

        TxEnqueue {
            accepted,
            sent_immediately: service.sent,
        }
    }

    pub fn drain_rx(&mut self, out: &mut [RxItem]) -> usize {
        let mut count = 0;
        for slot in out {
            let Some(item) = self.rx_fifo.pop_front() else {
                break;
            };
            *slot = item;
            count += 1;
        }
        count
    }

    pub fn rx_pending(&self) -> bool {
        !self.rx_fifo.is_empty()
    }

    pub fn handle_irq(&mut self) -> SerialIrqOutcome {
        let mut outcome = SerialIrqOutcome::default();

        if self.state != PortState::Up {
            return outcome;
        }

        let mut rx_budget = RX_IRQ_BUDGET;
        let mut tx_budget = TX_IRQ_BUDGET;

        for _ in 0..IRQ_PASS_BUDGET {
            let snapshot = self.raw.take_irq_snapshot();

            if !snapshot.claimed {
                if !outcome.claimed {
                    self.counters.irq_spurious += 1;
                }
                break;
            }

            if !outcome.claimed {
                self.counters.irq_total += 1;
            }
            outcome.claimed = true;

            if snapshot
                .sources
                .intersects(IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS)
            {
                let pushed = self.service_rx_locked(rx_budget);
                outcome.rx_pushed += pushed;
                rx_budget = rx_budget.saturating_sub(pushed);
            }

            if snapshot.sources.contains(IrqSource::TX_SPACE) {
                let service = self.service_tx_locked(tx_budget);
                outcome.tx_sent += service.sent;
                outcome.tx_wakeup |= service.wake_writers;
                tx_budget = tx_budget.saturating_sub(service.sent);
            }

            if snapshot.sources.contains(IrqSource::MODEM_STATUS) {
                self.raw.ack_modem_status();
            }

            if snapshot.sources.contains(IrqSource::BUSY_DETECT) {
                self.raw.ack_busy_detect();
            }

            if rx_budget == 0 || tx_budget == 0 {
                outcome.budget_exhausted = true;
                self.counters.irq_budget_exhausted += 1;
                break;
            }
        }

        outcome
    }

    pub fn startup_catch_up(&mut self) -> SerialIrqOutcome {
        if self.state != PortState::Up {
            return SerialIrqOutcome::default();
        }

        let rx_pushed = self.service_rx_locked(RX_IRQ_BUDGET);
        SerialIrqOutcome {
            claimed: false,
            rx_pushed,
            tx_sent: 0,
            tx_wakeup: false,
            budget_exhausted: rx_pushed == RX_IRQ_BUDGET,
        }
    }

    pub fn counters(&self) -> SerialCounters {
        self.counters
    }

    fn set_irq_mask_locked(&mut self, mask: InterruptMask) {
        if self.irq_mask != mask {
            self.raw.set_irq_mask(mask);
            self.irq_mask = mask;
        }
    }

    fn start_tx_irq_locked(&mut self) {
        if !self.irq_mask.contains(InterruptMask::TX_SPACE) {
            self.set_irq_mask_locked(self.irq_mask | InterruptMask::TX_SPACE);
        }
    }

    fn stop_tx_irq_locked(&mut self) {
        if self.irq_mask.contains(InterruptMask::TX_SPACE) {
            self.set_irq_mask_locked(self.irq_mask & !InterruptMask::TX_SPACE);
        }
    }

    fn service_tx_locked(&mut self, budget: usize) -> TxService {
        let before = self.tx_fifo.len();
        if before == 0 {
            self.stop_tx_irq_locked();
            return TxService::default();
        }

        let limit = budget.min(self.raw.tx_load_size().max(1));
        let mut sent = 0;
        while sent < limit && self.raw.tx_ready() {
            let Some(byte) = self.tx_fifo.front().copied() else {
                break;
            };
            self.raw.write_tx(byte);
            self.tx_fifo.pop_front();
            sent += 1;
            self.counters.tx_bytes += 1;
        }

        let remaining = self.tx_fifo.len();
        if remaining == 0 {
            self.stop_tx_irq_locked();
        } else {
            self.start_tx_irq_locked();
        }

        let wakeup_threshold = TX_WAKEUP_WATERMARK.min(TX.saturating_sub(1));
        TxService {
            sent,
            wake_writers: before == TX
                || (before > wakeup_threshold && remaining <= wakeup_threshold),
        }
    }

    fn service_rx_locked(&mut self, budget: usize) -> usize {
        let mut pushed = 0;

        for _ in 0..budget {
            let Some(sample) = self.raw.read_rx() else {
                break;
            };

            match sample.flag {
                RxFlag::Normal => {}
                RxFlag::Break => self.counters.rx_breaks += 1,
                RxFlag::Parity => self.counters.rx_parity_errors += 1,
                RxFlag::Framing => self.counters.rx_framing_errors += 1,
            }

            if let Some(byte) = sample.byte {
                self.counters.rx_bytes += 1;

                if self
                    .rx_fifo
                    .push_back(RxItem::Byte {
                        byte,
                        flag: sample.flag,
                    })
                    .is_ok()
                {
                    pushed += 1;
                } else {
                    self.counters.rx_queue_dropped += 1;
                }
            }

            if sample.overrun {
                self.counters.rx_fifo_overruns += 1;
                if self.rx_fifo.push_back(RxItem::Overrun).is_ok() {
                    pushed += 1;
                } else {
                    self.counters.rx_queue_dropped += 1;
                }
            }
        }

        pushed
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, vec::Vec};
    use core::num::NonZeroU32;

    use super::*;
    use crate::{DataBits, IrqSnapshot, Parity, RxSample, StopBits};

    struct MockUart {
        irq: VecDeque<IrqSnapshot>,
        rx: VecDeque<RxSample>,
        tx_ready_budget: usize,
        tx_written: Vec<u8>,
        mask: InterruptMask,
    }

    impl MockUart {
        fn new() -> Self {
            Self {
                irq: VecDeque::new(),
                rx: VecDeque::new(),
                tx_ready_budget: 0,
                tx_written: Vec::new(),
                mask: InterruptMask::empty(),
            }
        }

        fn irq(mut self, sources: IrqSource) -> Self {
            self.irq.push_back(IrqSnapshot {
                claimed: true,
                sources,
            });
            self
        }

        fn rx_byte(mut self, byte: u8) -> Self {
            self.rx.push_back(RxSample {
                byte: Some(byte),
                flag: RxFlag::Normal,
                overrun: false,
            });
            self
        }
    }

    impl RawUart for MockUart {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn base_addr(&self) -> usize {
            0x1000
        }

        fn clock_freq(&self) -> Option<NonZeroU32> {
            NonZeroU32::new(1)
        }

        fn startup(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn shutdown(&mut self) {}

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

        fn enable_loopback(&mut self) {}
        fn disable_loopback(&mut self) {}

        fn is_loopback_enabled(&self) -> bool {
            false
        }

        fn set_irq_mask(&mut self, mask: InterruptMask) {
            self.mask = mask;
        }

        fn take_irq_snapshot(&mut self) -> IrqSnapshot {
            self.irq.pop_front().unwrap_or_default()
        }

        fn read_rx(&mut self) -> Option<RxSample> {
            self.rx.pop_front()
        }

        fn tx_ready(&mut self) -> bool {
            self.tx_ready_budget > 0
        }

        fn write_tx(&mut self, byte: u8) {
            assert!(self.tx_ready_budget > 0);
            self.tx_ready_budget -= 1;
            self.tx_written.push(byte);
        }

        fn tx_load_size(&self) -> usize {
            16
        }

        fn tx_idle(&mut self) -> bool {
            self.tx_written.is_empty()
        }

        fn poll_status(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }
    }

    fn started_core<const TX: usize, const RX: usize>(
        uart: MockUart,
    ) -> SerialCore<MockUart, TX, RX> {
        let mut core = SerialCore::new(uart);
        core.startup(&Config::new()).unwrap();
        core
    }

    #[test]
    fn no_pending_irq_is_unhandled_even_with_buffered_rx() {
        let mut core = started_core::<16, 16>(MockUart::new());
        core.rx_fifo
            .push_back(RxItem::Byte {
                byte: b'x',
                flag: RxFlag::Normal,
            })
            .unwrap();

        let outcome = core.handle_irq();

        assert!(!outcome.claimed);
    }

    #[test]
    fn one_irq_services_rx_and_tx() {
        let mut core = started_core::<16, 16>(
            MockUart::new()
                .irq(IrqSource::RX_DATA | IrqSource::TX_SPACE)
                .rx_byte(b'Z'),
        );

        assert_eq!(core.enqueue_tx(b"abc").accepted, 3);
        core.raw_mut().tx_ready_budget = 3;
        let outcome = core.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, 1);
        assert!(outcome.tx_sent > 0);
    }

    #[test]
    fn tx_keeps_unsent_suffix() {
        let mut core = started_core::<16, 16>(MockUart::new().irq(IrqSource::TX_SPACE));

        assert_eq!(core.enqueue_tx(b"abcdef").accepted, 6);
        core.raw_mut().tx_ready_budget = 2;
        let outcome = core.handle_irq();

        assert_eq!(outcome.tx_sent, 2);
        assert_eq!(core.chars_in_buffer(), 4);
    }

    #[test]
    fn rx_irq_is_bounded() {
        let mut uart = MockUart::new().irq(IrqSource::RX_DATA);
        for _ in 0..(RX_IRQ_BUDGET + 8) {
            uart = uart.rx_byte(b'x');
        }
        let mut core = started_core::<16, 512>(uart);

        let outcome = core.handle_irq();

        assert!(outcome.budget_exhausted);
        assert!(outcome.rx_pushed <= RX_IRQ_BUDGET);
    }

    #[test]
    fn rx_full_queue_records_drops_but_drains_hardware() {
        let mut uart = MockUart::new().irq(IrqSource::RX_DATA);
        for _ in 0..8 {
            uart = uart.rx_byte(b'x');
        }
        let mut core = started_core::<16, 4>(uart);
        for _ in 0..4 {
            core.rx_fifo
                .push_back(RxItem::Byte {
                    byte: b'q',
                    flag: RxFlag::Normal,
                })
                .unwrap();
        }

        core.handle_irq();

        assert!(core.counters().rx_queue_dropped > 0);
    }

    #[test]
    fn rx_status_without_byte_is_preserved_as_queue_event() {
        let mut uart = MockUart::new().irq(IrqSource::RX_STATUS);
        uart.rx.push_back(RxSample {
            byte: None,
            flag: RxFlag::Parity,
            overrun: true,
        });
        let mut core = started_core::<16, 16>(uart);

        let outcome = core.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, 1);
        assert_eq!(core.counters().rx_parity_errors, 1);
        assert_eq!(core.counters().rx_fifo_overruns, 1);

        let mut items = [RxItem::default(); 1];
        assert_eq!(core.drain_rx(&mut items), 1);
        assert_eq!(items[0], RxItem::Overrun);
    }
}
