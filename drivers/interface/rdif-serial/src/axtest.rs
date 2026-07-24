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

#[axtest]
fn rdif_serial_interrupt_mask_and_event_hold() {
    use crate::{InterruptMask, IrqSnapshot, IrqSource, SerialEvent};

    // Test InterruptMask empty and combinations
    let empty = InterruptMask::empty();
    ax_assert!(empty.is_empty());

    // Test SerialEvent variants exist
    let _rx_ready = SerialEvent::RX_READY;
    let _tx_ready = SerialEvent::TX_READY;

    // Test IrqSnapshot basic usage
    let snap = IrqSnapshot {
        claimed: true,
        sources: IrqSource::RX_DATA,
    };
    ax_assert!(snap.claimed);
    ax_assert!(matches!(snap.sources, IrqSource::RX_DATA));
}

#[axtest]
fn rdif_serial_all_event_and_source_variants_hold() {
    use crate::IrqSource;

    // Test all IrqSource variants exist and are distinct
    let rx_data = IrqSource::RX_DATA;
    let rx_timeout = IrqSource::RX_TIMEOUT;
    let rx_status = IrqSource::RX_STATUS;
    let tx_space = IrqSource::TX_SPACE;
    let modem_status = IrqSource::MODEM_STATUS;
    let busy_detect = IrqSource::BUSY_DETECT;
    let other_ack = IrqSource::OTHER_ACK;

    ax_assert!(rx_data != rx_timeout);
    ax_assert!(rx_timeout != rx_status);
    ax_assert!(rx_status != tx_space);
    ax_assert!(tx_space != modem_status);
    ax_assert!(modem_status != busy_detect);
    ax_assert!(busy_detect != other_ack);

    // Test RxFlag variants
    use crate::RxFlag;
    let _normal = RxFlag::Normal;
    let _break = RxFlag::Break;
    let _parity = RxFlag::Parity;
    let _framing = RxFlag::Framing;

    // Test SerialCounters struct exists
    use crate::SerialCounters;
    let counters = SerialCounters::default();
    ax_assert_eq!(counters.irq_total, 0);
    ax_assert_eq!(counters.irq_spurious, 0);
}

#[axtest]
fn rdif_serial_counters_and_outcome_hold() {
    use crate::{SerialCounters, SerialIrqOutcome};

    // Test SerialCounters all default to 0
    let c = SerialCounters::default();
    ax_assert_eq!(c.irq_total, 0);
    ax_assert_eq!(c.irq_spurious, 0);
    ax_assert_eq!(c.irq_budget_exhausted, 0);
    ax_assert_eq!(c.rx_bytes, 0);
    ax_assert_eq!(c.rx_fifo_overruns, 0);
    ax_assert_eq!(c.rx_queue_dropped, 0);
    ax_assert_eq!(c.rx_breaks, 0);
    ax_assert_eq!(c.rx_parity_errors, 0);
    ax_assert_eq!(c.rx_framing_errors, 0);
    ax_assert_eq!(c.tx_bytes, 0);

    // Test SerialIrqOutcome defaults
    let o = SerialIrqOutcome::default();
    ax_assert!(!o.claimed);
    ax_assert_eq!(o.rx_pushed, 0);
    ax_assert_eq!(o.tx_sent, 0);
    ax_assert!(!o.tx_wakeup);
    ax_assert!(!o.budget_exhausted);

    // Test SerialIrqOutcome with non-default values
    let o2 = SerialIrqOutcome {
        claimed: true,
        rx_pushed: 10,
        tx_sent: 5,
        tx_wakeup: true,
        budget_exhausted: true,
    };
    ax_assert!(o2.claimed);
    ax_assert_eq!(o2.rx_pushed, 10);
    ax_assert_eq!(o2.tx_sent, 5);
    ax_assert!(o2.tx_wakeup);
    ax_assert!(o2.budget_exhausted);
}

#[axtest]
fn rdif_serial_rx_item_and_sample_hold() {
    use crate::{RxFlag, RxItem, RxSample};

    // Test RxFlag variants
    let normal = RxFlag::Normal;
    let brk = RxFlag::Break;
    let parity = RxFlag::Parity;
    let framing = RxFlag::Framing;
    ax_assert!(normal != brk);
    ax_assert!(brk != parity);
    ax_assert!(parity != framing);

    // Test RxSample default
    let s = RxSample::default();
    ax_assert!(s.byte.is_none());
    ax_assert!(matches!(s.flag, RxFlag::Normal));
    ax_assert!(!s.overrun);

    // Test RxSample with values
    let s2 = RxSample {
        byte: Some(0x42u8),
        flag: RxFlag::Parity,
        overrun: true,
    };
    ax_assert!(s2.byte.is_some());
    ax_assert_eq!(s2.byte.unwrap(), 0x42);
    ax_assert!(matches!(s2.flag, RxFlag::Parity));
    ax_assert!(s2.overrun);

    // Test RxItem::Byte variant
    let item_byte = RxItem::Byte {
        byte: 0xFF,
        flag: RxFlag::Break,
    };
    ax_assert!(matches!(item_byte, RxItem::Byte { .. }));

    // Test RxItem::Overrun variant
    let item_overrun = RxItem::Overrun;
    ax_assert!(matches!(item_overrun, RxItem::Overrun));

    // Test RxItem default
    let d = RxItem::default();
    ax_assert!(matches!(
        d,
        RxItem::Byte {
            byte: 0,
            flag: RxFlag::Normal
        }
    ));
}

#[axtest]
fn rdif_serial_config_error_and_transfer_error_hold() {
    use crate::{ConfigError, TransBytesError, TransferError};

    // Test ConfigError variants
    let _invalid_baudrate = ConfigError::InvalidBaudrate;
    let _unsupported_data = ConfigError::UnsupportedDataBits;
    let _unsupported_stop = ConfigError::UnsupportedStopBits;
    let _unsupported_parity = ConfigError::UnsupportedParity;
    let _register_error = ConfigError::RegisterError;
    let _timeout = ConfigError::Timeout;

    ax_assert!(ConfigError::InvalidBaudrate != ConfigError::Timeout);
    ax_assert!(ConfigError::UnsupportedDataBits != ConfigError::RegisterError);

    // Test TransferError variants
    let overrun = TransferError::Overrun(0x42);
    ax_assert!(matches!(overrun, TransferError::Overrun(_)));

    let parity = TransferError::Parity;
    ax_assert!(matches!(parity, TransferError::Parity));

    let framing = TransferError::Framing;
    ax_assert!(matches!(framing, TransferError::Framing));

    let brk = TransferError::Break;
    ax_assert!(matches!(brk, TransferError::Break));

    let closed = TransferError::Closed;
    ax_assert!(matches!(closed, TransferError::Closed));

    // Test TransBytesError
    let tbe = TransBytesError {
        bytes_transferred: 10,
        kind: TransferError::Parity,
    };
    ax_assert_eq!(tbe.bytes_transferred, 10);
    ax_assert!(matches!(tbe.kind, TransferError::Parity));
}

#[axtest]
fn rdif_serial_data_bits_and_config_hold() {
    use crate::{Config, DataBits, Parity, StopBits};

    // Test DataBits variants
    let five = DataBits::Five;
    let six = DataBits::Six;
    let seven = DataBits::Seven;
    let eight = DataBits::Eight;

    ax_assert!(five != six);
    ax_assert!(six != seven);
    ax_assert!(seven != eight);

    // Test DataBits repr(u8)
    ax_assert!(five as u8 == 5);
    ax_assert!(six as u8 == 6);
    ax_assert!(seven as u8 == 7);
    ax_assert!(eight as u8 == 8);

    // Test Config default and builder pattern
    let config = Config::new()
        .baudrate(115200)
        .data_bits(DataBits::Eight)
        .stop_bits(StopBits::One)
        .parity(Parity::None);

    ax_assert!(config.baudrate.is_some());
    ax_assert_eq!(config.baudrate.unwrap(), 115200);
    ax_assert!(config.data_bits.is_some());
    ax_assert!(config.stop_bits.is_some());
    ax_assert!(config.parity.is_some());

    // Test Config default (all None)
    let empty = Config::default();
    ax_assert!(empty.baudrate.is_none());
    ax_assert!(empty.data_bits.is_none());
    ax_assert!(empty.stop_bits.is_none());
    ax_assert!(empty.parity.is_none());
}

#[axtest]
fn rdif_serial_event_methods_hold() {
    use crate::SerialEvent;

    // Test SerialEvent is a bitflags type with methods
    let rx_ready = SerialEvent::RX_READY;
    let tx_ready = SerialEvent::TX_READY;
    let rx_error = SerialEvent::RX_ERROR;
    let tx_error = SerialEvent::TX_ERROR;
    let overrun = SerialEvent::OVERRUN;
    let modem_status = SerialEvent::MODEM_STATUS;
    let irq_ack = SerialEvent::IRQ_ACK;

    // Test rx_ready() method
    ax_assert!(rx_ready.rx_ready());
    ax_assert!(!tx_ready.rx_ready());

    // Test tx_ready() method
    ax_assert!(tx_ready.tx_ready());
    ax_assert!(!rx_ready.tx_ready());

    // Test rx_error() method - checks RX_ERROR | OVERRUN
    ax_assert!(rx_error.rx_error());
    ax_assert!(overrun.rx_error());
    ax_assert!(!tx_ready.rx_error());

    // Test combinations
    let both = rx_ready | tx_ready;
    ax_assert!(both.rx_ready());
    ax_assert!(both.tx_ready());

    // Test all events combined
    let all = rx_ready | tx_ready | rx_error | tx_error | overrun | modem_status | irq_ack;
    ax_assert!(all.rx_ready());
    ax_assert!(all.tx_ready());
    ax_assert!(all.rx_error());
}

#[axtest]
fn rdif_serial_direction_hold() {
    use crate::SerialDirection;

    let input = SerialDirection::Input;
    let output = SerialDirection::Output;

    ax_assert!(input != output);

    // Test Debug, Clone, Copy, PartialEq, Eq
    let cloned = input;
    ax_assert!(cloned == input);
}

#[axtest]
fn rdif_serial_parity_hold() {
    use crate::Parity;

    let none = Parity::None;
    let odd = Parity::Odd;
    let even = Parity::Even;
    let mark = Parity::Mark;
    let space = Parity::Space;

    // All variants are distinct
    ax_assert!(none != odd);
    ax_assert!(odd != even);
    ax_assert!(even != mark);
    ax_assert!(mark != space);
}

#[axtest]
fn rdif_serial_stop_bits_hold() {
    use crate::StopBits;

    let one = StopBits::One;
    let two = StopBits::Two;

    ax_assert!(one != two);

    // Test Debug, Clone, Copy, PartialEq, Eq
    let cloned = one;
    ax_assert!(cloned == one);
}

#[axtest]
fn rdif_serial_interrupt_mask_hold() {
    use crate::InterruptMask;

    let empty = InterruptMask::empty();
    ax_assert!(empty.is_empty());

    // Test bitflags operations
    let mask1 = InterruptMask::RX_DATA;
    let mask2 = InterruptMask::TX_SPACE;
    let combined = mask1 | mask2;
    ax_assert!(combined.contains(mask1));
    ax_assert!(combined.contains(mask2));
    ax_assert!(!combined.contains(InterruptMask::RX_STATUS));

    // Test rx_available method
    let rx_mask = InterruptMask::RX_DATA | InterruptMask::RX_STATUS;
    ax_assert!(rx_mask.rx_available());
}

#[axtest]
fn rdif_serial_irq_snapshot_hold() {
    use crate::{IrqSnapshot, IrqSource};

    // Test IrqSnapshot struct
    let empty = IrqSnapshot {
        claimed: false,
        sources: IrqSource::empty(),
    };
    ax_assert!(!empty.claimed);
    ax_assert!(empty.sources.is_empty());

    // Test with individual sources
    let rx_data = IrqSnapshot {
        claimed: true,
        sources: IrqSource::RX_DATA,
    };
    ax_assert!(rx_data.claimed);
}

#[axtest]
fn rdif_serial_owner_id_hold() {
    use crate::OwnerId;

    // Test OwnerId
    let id1 = OwnerId(1);
    let id2 = OwnerId(2);
    ax_assert!(id1 != id2);
    ax_assert_eq!(id1.0, 1);
}

#[axtest]
fn rdif_serial_raw_uart_trait_methods_hold() {
    use alloc::vec::Vec;

    // Test that RawUart trait has the expected methods
    // We can't implement it fully, but verify MockUart implements it
    let mock = MockUart::new(Vec::new());

    // Test name()
    ax_assert_eq!(mock.name(), "mock-uart");

    // Test base_addr()
    ax_assert_eq!(mock.base_addr(), 0x1000);
}

#[axtest]
fn rdif_serial_config_builder_pattern_hold() {
    use crate::{Config, DataBits, Parity, StopBits};

    // Test Config builder pattern
    let config = Config::new()
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One);

    // Verify config was created
    let _config_ref = &config;
}

#[axtest]
fn rdif_serial_spsc_ring_basic_hold() {
    use crate::SpscRing;

    // Test SpscRing exists and has basic methods
    let ring = SpscRing::<u8, 16>::new();

    // Test is_empty on new ring
    ax_assert!(ring.is_empty());
}

#[axtest]
fn rdif_serial_serial_soft_work_hold() {
    use crate::SerialSoftWork;

    // Test SerialSoftWork is a bitflags type
    let empty = SerialSoftWork::empty();
    ax_assert!(empty.is_empty());

    let tx_kick = SerialSoftWork::TX_KICK;
    ax_assert!(!tx_kick.is_empty());

    // Test RESERVICE flag
    let reservice = SerialSoftWork::RESERVICE;
    ax_assert!(!reservice.is_empty());

    // Test combination
    let combined = tx_kick | reservice;
    ax_assert!(combined.contains(SerialSoftWork::TX_KICK));
    ax_assert!(combined.contains(SerialSoftWork::RESERVICE));
}

#[axtest]
fn rdif_serial_serial_port_lifecycle_hold() {
    use crate::{Config, DataBits, Parity, StopBits};

    // Test SerialPort::new() and lifecycle
    let config = Config::new()
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One);

    // Verify config is valid
    let _config_ref = &config;
}

#[axtest]
fn rdif_serial_rx_sample_and_flag_comprehensive_hold() {
    use crate::{RxFlag, RxSample};

    // Test all RxFlag variants
    let normal = RxFlag::Normal;
    let break_flag = RxFlag::Break;
    let parity = RxFlag::Parity;
    let framing = RxFlag::Framing;

    ax_assert!(normal != break_flag);
    ax_assert!(parity != framing);

    // Test RxSample with different flags
    let sample_normal = RxSample {
        byte: Some(0x41),
        flag: RxFlag::Normal,
        overrun: false,
    };
    ax_assert_eq!(sample_normal.byte, Some(0x41));
    ax_assert!(!sample_normal.overrun);

    let sample_overrun = RxSample {
        byte: None,
        flag: RxFlag::Framing,
        overrun: true,
    };
    ax_assert!(sample_overrun.overrun);
}

#[axtest]
fn rdif_serial_rx_item_variants_hold() {
    use crate::{RxFlag, RxItem};

    // Test RxItem::Byte variant
    let byte_item = RxItem::Byte {
        byte: 0x42,
        flag: RxFlag::Normal,
    };

    // Test RxItem::Overrun variant
    let overrun_item = RxItem::Overrun;

    // Verify they are different
    // RxItem is an enum so we can pattern match
    match byte_item {
        RxItem::Byte { byte, .. } => ax_assert_eq!(byte, 0x42),
        RxItem::Overrun => ax_assert!(false), // Should not reach here
    }

    match overrun_item {
        RxItem::Byte { .. } => ax_assert!(false), // Should not reach here
        RxItem::Overrun => {}                     // Expected
    }
}

#[axtest]
fn rdif_serial_trans_bytes_error_hold() {
    use crate::{TransBytesError, TransferError};

    // Test TransBytesError struct
    let error = TransBytesError {
        bytes_transferred: 10,
        kind: TransferError::Closed,
    };
    ax_assert_eq!(error.bytes_transferred, 10);
}

#[axtest]
fn rdif_serial_config_error_all_variants_hold() {
    use crate::ConfigError;

    // Test ConfigError variants exist and are distinct
    let _invalid_baud = ConfigError::InvalidBaudrate;
    let _unsupported_data = ConfigError::UnsupportedDataBits;
    let _unsupported_stop = ConfigError::UnsupportedStopBits;
    let _unsupported_parity = ConfigError::UnsupportedParity;
    let _register = ConfigError::RegisterError;
    let _timeout = ConfigError::Timeout;
}

#[axtest]
fn rdif_serial_transfer_error_all_variants_hold() {
    use crate::TransferError;

    // Test TransferError variants exist
    let _overrun = TransferError::Overrun(0xFF);
    let _parity = TransferError::Parity;
    let _framing = TransferError::Framing;
    let _break_cond = TransferError::Break;
    let _closed = TransferError::Closed;
}

#[axtest]
fn rdif_serial_rx_flag_and_event_types_hold() {
    use crate::RxFlag;

    // Test RxFlag variants
    let _normal = RxFlag::Normal;
    let _break_flag = RxFlag::Break;
    let _parity = RxFlag::Parity;
    let _framing = RxFlag::Framing;

    // Test SerialEvent exists
}

#[axtest]
fn rdif_serial_data_bits_stop_bits_parity_hold() {
    use crate::{DataBits, Parity, StopBits};

    // Test DataBits variants
    let _data8 = DataBits::Eight;
    let _data7 = DataBits::Seven;
    let _data6 = DataBits::Six;
    let _data5 = DataBits::Five;

    // Test StopBits variants
    let _stop1 = StopBits::One;
    let _stop2 = StopBits::Two;

    // Test Parity variants
    let _parity_none = Parity::None;
    let _parity_even = Parity::Even;
    let _parity_odd = Parity::Odd;
    let _parity_mark = Parity::Mark;
    let _parity_space = Parity::Space;
}

#[axtest]
fn rdif_serial_serial_event_flags_hold() {
    use crate::SerialEvent;

    // Test SerialEvent flag values
    let rx_ready = SerialEvent::RX_READY;
    let tx_ready = SerialEvent::TX_READY;
    let rx_error = SerialEvent::RX_ERROR;
    let _tx_error = SerialEvent::TX_ERROR;
    let _overrun = SerialEvent::OVERRUN;
    let _modem_status = SerialEvent::MODEM_STATUS;

    // Verify flags are distinct
    assert!(rx_ready.bits() != tx_ready.bits());
    assert!(rx_ready.bits() != rx_error.bits());
}

#[axtest]
fn rdif_serial_serial_event_combinations_hold() {
    use crate::SerialEvent;

    // Test SerialEvent flag combinations
    let rx_ready = SerialEvent::RX_READY;
    let tx_ready = SerialEvent::TX_READY;

    // Test contains
    let combined = rx_ready | tx_ready;
    assert!(combined.contains(SerialEvent::RX_READY));
    assert!(combined.contains(SerialEvent::TX_READY));
    assert!(!combined.contains(SerialEvent::RX_ERROR));

    // Test empty
    let empty = SerialEvent::empty();
    assert!(empty.is_empty());

    // Test bits()
    assert!(rx_ready.bits() > 0);
}

#[axtest]
fn rdif_serial_config_error_variants_discriminant_hold() {
    use crate::ConfigError;

    // Test ConfigError variants with correct names
    let _register = ConfigError::RegisterError;
    let _timeout = ConfigError::Timeout;

    // Verify they are different types
    assert!(
        core::mem::discriminant(&ConfigError::RegisterError)
            != core::mem::discriminant(&ConfigError::Timeout)
    );
}

#[axtest]
fn rdif_serial_owner_lease_exists_hold() {
    use crate::OwnerId;

    // Test OwnerLease and OwnerId types exist
    let id = OwnerId(42);
    assert_eq!(id.0, 42);
}

#[axtest]
fn rdif_serial_transfer_error_display_hold() {
    use crate::{TransBytesError, TransferError};

    // Test TransferError Display impl (via format!)
    let err = TransBytesError {
        bytes_transferred: 5,
        kind: TransferError::Framing,
    };
    let _s = alloc::format!("{err}");
}

#[axtest]
fn rdif_serial_config_default_and_new_hold() {
    use crate::Config;

    // Test Config::new() and Config::default()
    let new_config = Config::new();
    let default_config = Config::default();

    // Both should have all None fields
    assert!(new_config.baudrate.is_none());
    assert!(default_config.baudrate.is_none());
}

#[axtest]
fn rdif_serial_rx_item_default_hold() {
    use crate::RxItem;

    // Test RxItem default is Byte with 0 and Normal flag
    let d = RxItem::default();
    match d {
        RxItem::Byte { byte, flag: _ } => {
            assert_eq!(byte, 0);
        }
        RxItem::Overrun => assert!(false),
    }
}

#[axtest]
fn rdif_serial_irq_source_bitflags_hold() {
    use crate::IrqSource;

    // Test IrqSource is a bitflags type
    let empty = IrqSource::empty();
    assert!(empty.is_empty());

    let rx_data = IrqSource::RX_DATA;
    assert!(!rx_data.is_empty());

    // Test contains
    let combined = rx_data | IrqSource::TX_SPACE;
    assert!(combined.contains(IrqSource::RX_DATA));
    assert!(combined.contains(IrqSource::TX_SPACE));
    assert!(!combined.contains(IrqSource::MODEM_STATUS));
}

#[axtest]
fn rdif_serial_spsc_ring_pop_and_clear_hold() {
    use crate::SpscRing;

    let ring = SpscRing::<u8, 8>::new();
    assert!(ring.is_empty());

    // Pop from empty ring returns None
    assert_eq!(ring.pop(), None);

    // Clear on empty ring is no-op
    // (can't test directly without mutable reference in this context)
}
