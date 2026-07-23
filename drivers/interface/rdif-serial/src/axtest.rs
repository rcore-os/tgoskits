use alloc::{boxed::Box, vec::Vec};
use core::num::NonZeroU32;

use axtest::prelude::*;

use crate::{
    Config, ConfigError, DataBits, InterruptMask, IrqSnapshot, IrqSource, OwnerId, OwnerLease,
    Parity, RawUart, RxFlag, RxItem, RxSample, SerialEvent, SerialPort, SerialSoftWork, SpscRing,
    StopBits, TransBytesError, TransferError,
};

struct MockUart {
    loopback: bool,
    mask: InterruptMask,
    rx: Vec<RxSample>,
    tx: Vec<u8>,
}

impl MockUart {
    fn new(rx: Vec<RxSample>) -> Self {
        Self {
            loopback: false,
            mask: InterruptMask::empty(),
            rx,
            tx: Vec::new(),
        }
    }
}

impl RawUart for MockUart {
    fn name(&self) -> &'static str {
        "mock-uart"
    }

    fn base_addr(&self) -> usize {
        0x1000
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(24_000_000)
    }

    fn startup(&mut self, _config: &Config) -> Result<(), ConfigError> {
        self.mask = InterruptMask::empty();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.mask = InterruptMask::empty();
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

    fn enable_loopback(&mut self) {
        self.loopback = true;
    }

    fn disable_loopback(&mut self) {
        self.loopback = false;
    }

    fn is_loopback_enabled(&self) -> bool {
        self.loopback
    }

    fn set_irq_mask(&mut self, mask: InterruptMask) {
        self.mask = mask;
    }

    fn take_irq_snapshot(&mut self) -> IrqSnapshot {
        IrqSnapshot {
            claimed: !self.mask.is_empty(),
            sources: IrqSource::RX_DATA | IrqSource::TX_SPACE,
        }
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        if self.rx.is_empty() {
            None
        } else {
            Some(self.rx.remove(0))
        }
    }

    fn tx_ready(&mut self) -> bool {
        true
    }

    fn write_tx(&mut self, byte: u8) {
        self.tx.push(byte);
    }

    fn poll_status(&mut self) -> SerialEvent {
        SerialEvent::RX_READY | SerialEvent::TX_READY
    }

    fn tx_idle(&mut self) -> bool {
        self.tx.is_empty()
    }
}

#[axtest]
fn rdif_serial_config_events_and_error_types_are_stable() {
    let config = Config::new()
        .baudrate(9_600)
        .data_bits(DataBits::Seven)
        .stop_bits(StopBits::Two)
        .parity(Parity::Even);

    ax_assert_eq!(config.baudrate, Some(9_600));
    ax_assert_eq!(config.data_bits, Some(DataBits::Seven));
    ax_assert_eq!(config.stop_bits, Some(StopBits::Two));
    ax_assert_eq!(config.parity, Some(Parity::Even));

    let event = SerialEvent::RX_READY | SerialEvent::OVERRUN;
    ax_assert!(event.rx_ready());
    ax_assert!(!event.tx_ready());
    ax_assert!(event.rx_error());

    let err = TransBytesError {
        bytes_transferred: 2,
        kind: TransferError::Parity,
    };
    ax_assert_eq!(err.bytes_transferred, 2);
    ax_assert!(alloc::format!("{err}").contains("Parity error"));
}

#[axtest]
fn rdif_serial_bitflags_and_rx_items_report_status() {
    let mask = InterruptMask::RX_DATA | InterruptMask::TX_SPACE;
    ax_assert!(mask.rx_available());
    ax_assert!(mask.tx_empty());

    let source = IrqSource::RX_TIMEOUT | IrqSource::BUSY_DETECT;
    ax_assert!(source.contains(IrqSource::RX_TIMEOUT));
    ax_assert!(source.contains(IrqSource::BUSY_DETECT));

    let sample = RxSample {
        byte: Some(b'x'),
        flag: RxFlag::Framing,
        overrun: false,
    };
    ax_assert_eq!(sample.byte, Some(b'x'));
    ax_assert_eq!(sample.flag, RxFlag::Framing);
    ax_assert_eq!(
        RxItem::Byte {
            byte: b'y',
            flag: RxFlag::Break
        },
        RxItem::Byte {
            byte: b'y',
            flag: RxFlag::Break
        }
    );
    ax_assert_eq!(RxItem::Overrun, RxItem::Overrun);
}

#[axtest]
fn rdif_serial_ring_keeps_reserved_slot_and_peek_is_non_consuming() {
    let ring = SpscRing::<u8, 4>::new();

    ax_assert_eq!(ring.capacity(), 3);
    ax_assert!(ring.is_empty());
    ax_assert_eq!(ring.push(1), Ok(()));
    ax_assert_eq!(ring.push(2), Ok(()));
    ax_assert_eq!(ring.push(3), Ok(()));
    ax_assert_eq!(ring.push(4), Err(4));
    ax_assert_eq!(ring.len_snapshot(), 3);
    ax_assert_eq!(ring.remaining_snapshot(), 0);
    ax_assert_eq!(ring.peek_copy(), Some(1));
    ax_assert_eq!(ring.peek_copy(), Some(1));
    ax_assert_eq!(ring.pop(), Some(1));
    ring.clear_consumer();
    ax_assert!(ring.is_empty());
}

#[axtest]
fn rdif_serial_raw_uart_default_helpers_translate_rx_samples() {
    let mut uart = MockUart::new(alloc::vec![
        RxSample {
            byte: Some(b'a'),
            flag: RxFlag::Normal,
            overrun: false,
        },
        RxSample {
            byte: Some(b'b'),
            flag: RxFlag::Parity,
            overrun: false,
        },
        RxSample {
            byte: Some(b'c'),
            flag: RxFlag::Normal,
            overrun: true,
        },
    ]);

    ax_assert_eq!(uart.name(), "mock-uart");
    ax_assert_eq!(uart.base_addr(), 0x1000);
    ax_assert_eq!(uart.clock_freq().unwrap().get(), 24_000_000);
    uart.startup(&Config::new()).unwrap();
    uart.set_irq_mask(InterruptMask::RX_DATA);
    ax_assert!(uart.take_irq_snapshot().claimed);
    uart.enable_loopback();
    ax_assert!(uart.is_loopback_enabled());
    uart.disable_loopback();
    ax_assert!(!uart.is_loopback_enabled());

    ax_assert_eq!(
        uart.read_byte(SerialEvent::RX_READY).unwrap().unwrap(),
        b'a'
    );
    ax_assert_eq!(
        uart.read_byte(SerialEvent::RX_READY).unwrap(),
        Err(TransferError::Parity)
    );
    ax_assert_eq!(
        uart.read_byte(SerialEvent::RX_READY).unwrap(),
        Err(TransferError::Overrun(b'c'))
    );
    ax_assert_eq!(uart.read_byte(SerialEvent::empty()), None);
    uart.write_byte(b'z');
    ax_assert_eq!(uart.tx, alloc::vec![b'z']);
}

#[axtest]
fn rdif_serial_boxed_raw_uart_delegates_to_inner_driver() {
    let mut uart: Box<dyn RawUart> = Box::new(MockUart::new(Vec::new()));

    ax_assert_eq!(uart.name(), "mock-uart");
    ax_assert_eq!(uart.base_addr(), 0x1000);
    ax_assert_eq!(uart.clock_freq().unwrap().get(), 24_000_000);
    uart.startup(&Config::new()).unwrap();
    uart.set_config(&Config::new().baudrate(115_200)).unwrap();
    ax_assert_eq!(uart.baudrate(), 115_200);
    ax_assert_eq!(uart.data_bits(), DataBits::Eight);
    ax_assert_eq!(uart.stop_bits(), StopBits::One);
    ax_assert_eq!(uart.parity(), Parity::None);
    uart.enable_loopback();
    ax_assert!(uart.is_loopback_enabled());
    uart.disable_loopback();
    ax_assert!(!uart.is_loopback_enabled());
    uart.set_irq_mask(InterruptMask::RX_DATA);
    ax_assert!(uart.take_irq_snapshot().claimed);
    uart.write_tx(b'q');
    ax_assert!(uart.tx_ready());
    ax_assert_eq!(
        uart.poll_status(),
        SerialEvent::RX_READY | SerialEvent::TX_READY
    );
    ax_assert_eq!(uart.tx_load_size(), 1);
    ax_assert!(!uart.tx_idle());
    uart.ack_modem_status();
    uart.ack_busy_detect();
    uart.shutdown();
}

#[axtest]
fn rdif_serial_raw_uart_default_read_byte_reports_remaining_error_flags() {
    let mut uart = MockUart::new(alloc::vec![
        RxSample {
            byte: Some(b'b'),
            flag: RxFlag::Break,
            overrun: false,
        },
        RxSample {
            byte: Some(b'f'),
            flag: RxFlag::Framing,
            overrun: false,
        },
        RxSample {
            byte: None,
            flag: RxFlag::Normal,
            overrun: false,
        },
    ]);

    ax_assert_eq!(
        uart.read_byte(SerialEvent::RX_ERROR).unwrap(),
        Err(TransferError::Break)
    );
    ax_assert_eq!(
        uart.read_byte(SerialEvent::RX_ERROR).unwrap(),
        Err(TransferError::Framing)
    );
    ax_assert_eq!(uart.read_byte(SerialEvent::RX_READY), None);
    ax_assert_eq!(uart.read_byte(SerialEvent::RX_ERROR), None);
}

struct RuntimeUart {
    irq: Vec<IrqSnapshot>,
    rx: Vec<RxSample>,
    tx_ready_budget: usize,
    tx: Vec<u8>,
    mask: InterruptMask,
    started: bool,
}

impl RuntimeUart {
    fn new() -> Self {
        Self {
            irq: Vec::new(),
            rx: Vec::new(),
            tx_ready_budget: 0,
            tx: Vec::new(),
            mask: InterruptMask::empty(),
            started: false,
        }
    }

    fn with_irq(mut self, sources: IrqSource) -> Self {
        self.irq.push(IrqSnapshot {
            claimed: true,
            sources,
        });
        self
    }

    fn with_rx(mut self, sample: RxSample) -> Self {
        self.rx.push(sample);
        self
    }

    fn ready_for_tx(mut self, budget: usize) -> Self {
        self.tx_ready_budget = budget;
        self
    }
}

impl RawUart for RuntimeUart {
    fn name(&self) -> &'static str {
        "runtime-uart"
    }

    fn base_addr(&self) -> usize {
        0x2000
    }

    fn clock_freq(&self) -> Option<NonZeroU32> {
        NonZeroU32::new(1_843_200)
    }

    fn startup(&mut self, _config: &Config) -> Result<(), ConfigError> {
        self.started = true;
        Ok(())
    }

    fn shutdown(&mut self) {
        self.started = false;
    }

    fn set_config(&mut self, _config: &Config) -> Result<(), ConfigError> {
        Ok(())
    }

    fn baudrate(&self) -> u32 {
        57_600
    }

    fn data_bits(&self) -> DataBits {
        DataBits::Seven
    }

    fn stop_bits(&self) -> StopBits {
        StopBits::Two
    }

    fn parity(&self) -> Parity {
        Parity::Odd
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
        if self.irq.is_empty() {
            IrqSnapshot::default()
        } else {
            self.irq.remove(0)
        }
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        if self.rx.is_empty() {
            None
        } else {
            Some(self.rx.remove(0))
        }
    }

    fn tx_ready(&mut self) -> bool {
        self.tx_ready_budget > 0
    }

    fn write_tx(&mut self, byte: u8) {
        self.tx_ready_budget -= 1;
        self.tx.push(byte);
    }

    fn poll_status(&mut self) -> SerialEvent {
        SerialEvent::empty()
    }

    fn tx_idle(&mut self) -> bool {
        self.tx.is_empty()
    }

    fn ack_modem_status(&mut self) {}

    fn ack_busy_detect(&mut self) {}
}

fn serial_owner_lease() -> OwnerLease<'static> {
    unsafe { OwnerLease::new_unchecked(OwnerId(7)) }
}

#[axtest]
fn rdif_serial_port_startup_service_shutdown_and_counters_hold() {
    let uart = RuntimeUart::new().ready_for_tx(2);
    let parts = SerialPort::<8, 8>::split(uart, OwnerId(7));
    ax_assert_eq!(parts.port.owner(), OwnerId(7));
    ax_assert_eq!(parts.irq.owner(), OwnerId(7));
    ax_assert_eq!(parts.tx.write_room(), 7);
    ax_assert!(!parts.rx.rx_pending());

    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();
    ax_assert_eq!(
        parts
            .port
            .startup(serial_owner_lease(), &Config::new())
            .unwrap(),
        crate::SerialIrqOutcome::default()
    );
    ax_assert_eq!(parts.port.baudrate(serial_owner_lease()), 57_600);
    parts
        .port
        .set_config(serial_owner_lease(), &Config::new().baudrate(38_400))
        .unwrap();

    let mut tx = parts.tx;
    ax_assert_eq!(tx.submit(b"abc").accepted, 3);
    ax_assert!(!parts.port.tx_idle(serial_owner_lease()));
    let outcome = parts
        .port
        .service(serial_owner_lease(), SerialSoftWork::TX_KICK);
    ax_assert_eq!(outcome.tx_sent, 2);
    ax_assert!(outcome.tx_wakeup);
    ax_assert_eq!(tx.chars_in_buffer(), 1);

    let counters = parts.port.counters();
    ax_assert_eq!(counters.tx_bytes, 2);
    parts.port.shutdown(serial_owner_lease());
    ax_assert_eq!(tx.chars_in_buffer(), 0);
}

#[axtest]
fn rdif_serial_irq_handler_publishes_rx_items_and_error_counters() {
    let uart = RuntimeUart::new()
        .with_irq(IrqSource::RX_DATA | IrqSource::MODEM_STATUS | IrqSource::BUSY_DETECT)
        .with_rx(RxSample {
            byte: Some(b'a'),
            flag: RxFlag::Break,
            overrun: false,
        })
        .with_rx(RxSample {
            byte: Some(b'b'),
            flag: RxFlag::Parity,
            overrun: false,
        })
        .with_rx(RxSample {
            byte: Some(b'c'),
            flag: RxFlag::Framing,
            overrun: true,
        });
    let parts = SerialPort::<8, 8>::split(uart, OwnerId(7));
    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();

    let mut irq = parts.irq;
    let outcome = irq.handle(serial_owner_lease());
    ax_assert!(outcome.claimed);
    ax_assert_eq!(outcome.rx_pushed, 4);

    let mut rx = parts.rx;
    ax_assert!(rx.rx_pending());
    let mut items = [RxItem::default(); 4];
    ax_assert_eq!(rx.drain(&mut items), 4);
    ax_assert_eq!(
        items,
        [
            RxItem::Byte {
                byte: b'a',
                flag: RxFlag::Break,
            },
            RxItem::Byte {
                byte: b'b',
                flag: RxFlag::Parity,
            },
            RxItem::Byte {
                byte: b'c',
                flag: RxFlag::Framing,
            },
            RxItem::Overrun,
        ]
    );

    let counters = parts.port.counters();
    ax_assert_eq!(counters.irq_total, 1);
    ax_assert_eq!(counters.rx_bytes, 3);
    ax_assert_eq!(counters.rx_breaks, 1);
    ax_assert_eq!(counters.rx_parity_errors, 1);
    ax_assert_eq!(counters.rx_framing_errors, 1);
    ax_assert_eq!(counters.rx_fifo_overruns, 1);
}

#[axtest]
fn rdif_serial_irq_handler_counts_spurious_and_budget_exhausted_paths() {
    let parts = SerialPort::<8, 8>::split(RuntimeUart::new(), OwnerId(7));
    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();
    let mut irq = parts.irq;

    let spurious = irq.handle(serial_owner_lease());
    ax_assert!(!spurious.claimed);
    ax_assert_eq!(parts.port.counters().irq_spurious, 1);

    let mut uart = RuntimeUart::new().with_irq(IrqSource::RX_DATA);
    for index in 0..(crate::RX_IRQ_BUDGET + 1) {
        uart = uart.with_rx(RxSample {
            byte: Some(index as u8),
            flag: RxFlag::Normal,
            overrun: false,
        });
    }
    let parts = SerialPort::<8, 512>::split(uart, OwnerId(7));
    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();
    let mut irq = parts.irq;
    let exhausted = irq.handle(serial_owner_lease());
    ax_assert!(exhausted.budget_exhausted);
    ax_assert_eq!(exhausted.rx_pushed, crate::RX_IRQ_BUDGET);
    ax_assert_eq!(parts.port.counters().irq_budget_exhausted, 1);
}

#[axtest]
fn rdif_serial_port_reservice_handles_rx_and_tx_without_hardware_irq() {
    let uart = RuntimeUart::new().ready_for_tx(3).with_rx(RxSample {
        byte: Some(b'r'),
        flag: RxFlag::Normal,
        overrun: false,
    });
    let parts = SerialPort::<8, 8>::split(uart, OwnerId(7));
    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();
    let mut tx = parts.tx;
    ax_assert_eq!(tx.submit(b"xy").accepted, 2);

    let outcome = parts
        .port
        .service(serial_owner_lease(), SerialSoftWork::RESERVICE);
    ax_assert_eq!(outcome.rx_pushed, 1);
    ax_assert_eq!(outcome.tx_sent, 2);
    ax_assert!(outcome.tx_wakeup);
    ax_assert_eq!(tx.chars_in_buffer(), 0);

    let mut rx = parts.rx;
    let mut item = [RxItem::default(); 1];
    ax_assert_eq!(rx.drain(&mut item), 1);
    ax_assert_eq!(
        item[0],
        RxItem::Byte {
            byte: b'r',
            flag: RxFlag::Normal,
        }
    );
}

#[axtest]
fn rdif_serial_irq_handler_services_tx_space_from_queue() {
    let uart = RuntimeUart::new()
        .ready_for_tx(1)
        .with_irq(IrqSource::TX_SPACE);
    let parts = SerialPort::<8, 8>::split(uart, OwnerId(7));
    parts
        .port
        .startup(serial_owner_lease(), &Config::new())
        .unwrap();
    let mut tx = parts.tx;
    ax_assert_eq!(tx.submit(b"pq").accepted, 2);

    let mut irq = parts.irq;
    let outcome = irq.handle(serial_owner_lease());
    ax_assert!(outcome.claimed);
    ax_assert_eq!(outcome.tx_sent, 1);
    ax_assert!(outcome.tx_wakeup);
    ax_assert_eq!(tx.chars_in_buffer(), 1);
    ax_assert_eq!(parts.port.counters().tx_bytes, 1);
}

#[axtest]
fn rdif_serial_config_data_bits_parity_stopbits_hold() {
    // DataBits variants - just check they exist and are distinct
    let five = DataBits::Five;
    let six = DataBits::Six;
    let seven = DataBits::Seven;
    let eight = DataBits::Eight;
    ax_assert!(five != six);
    ax_assert!(six != seven);
    ax_assert!(seven != eight);
    
    // Parity variants - just check they exist and are distinct
    let none = Parity::None;
    let odd = Parity::Odd;
    let even = Parity::Even;
    ax_assert!(none != odd);
    ax_assert!(odd != even);
    
    // StopBits variants - just check they exist and are distinct
    let one = StopBits::One;
    let two = StopBits::Two;
    ax_assert!(one != two);
}
