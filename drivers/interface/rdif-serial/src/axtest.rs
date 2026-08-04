use axtest::prelude::*;

use crate::{
    Config, ConfigError, DataBits, IrqRxSink, Parity, RxErrorFlags, RxFlag, RxSample,
    SerialEventSet, SerialIrqEvent, SplitUart, StopBits, UartInfo, UartIrq, UartParts, UartPort,
};

#[axtest]
fn config_builder_records_requested_serial_mode() {
    let config = Config::new()
        .baudrate(9_600)
        .data_bits(DataBits::Seven)
        .stop_bits(StopBits::Two)
        .parity(Parity::Even);

    ax_assert_eq!(config.baudrate, Some(9_600));
    ax_assert_eq!(config.data_bits, Some(DataBits::Seven));
    ax_assert_eq!(config.stop_bits, Some(StopBits::Two));
    ax_assert_eq!(config.parity, Some(Parity::Even));
}

#[axtest]
fn event_sets_classify_rx_and_tx_sources() {
    let rx = SerialEventSet::RX_DATA | SerialEventSet::RX_TIMEOUT;
    ax_assert!(rx.has_rx());
    ax_assert!(!rx.has_tx());

    let tx = SerialEventSet::TX_SPACE;
    ax_assert!(!tx.has_rx());
    ax_assert!(tx.has_tx());

    let combined = rx | tx | SerialEventSet::FAULT;
    ax_assert!(combined.contains(SerialEventSet::FAULT));
    ax_assert!(combined.has_rx());
    ax_assert!(combined.has_tx());
}

#[axtest]
fn rx_samples_and_irq_events_keep_normalized_status() {
    let sample = RxSample {
        byte: Some(b'x'),
        flag: RxFlag::Framing,
        overrun: true,
    };

    ax_assert_eq!(sample.byte, Some(b'x'));
    ax_assert_eq!(sample.flag, RxFlag::Framing);
    ax_assert!(sample.overrun);

    let event = SerialIrqEvent {
        events: SerialEventSet::RX_STATUS,
        rx_errors: RxErrorFlags::FRAMING | RxErrorFlags::OVERRUN,
        rearm: SerialEventSet::RX_DATA,
    };

    ax_assert!(event.events.has_rx());
    ax_assert!(event.rx_errors.contains(RxErrorFlags::FRAMING));
    ax_assert!(event.rx_errors.contains(RxErrorFlags::OVERRUN));
    ax_assert_eq!(event.rearm, SerialEventSet::RX_DATA);
}

struct MockPort {
    config: Config,
    tx: [u8; 4],
    tx_len: usize,
    rx: Option<RxSample>,
    masked: bool,
}

impl MockPort {
    const fn new() -> Self {
        Self {
            config: Config::new(),
            tx: [0; 4],
            tx_len: 0,
            rx: None,
            masked: false,
        }
    }
}

impl UartPort for MockPort {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.config = config.clone();
        self.masked = true;
        Ok(())
    }

    fn shutdown(&mut self) {
        self.masked = true;
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        self.config = config.clone();
        Ok(())
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        self.rx.take()
    }

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        let count = bytes.len().min(self.tx.len());
        self.tx[..count].copy_from_slice(&bytes[..count]);
        self.tx_len = count;
        count
    }

    fn tx_idle(&mut self) -> bool {
        self.tx_len == 0
    }

    fn mask_all(&mut self) {
        self.masked = true;
    }

    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet {
        self.masked = false;
        sources & SerialEventSet::RX
    }
}

struct MockIrq {
    pending: Option<SerialIrqEvent>,
    sample: Option<RxSample>,
}

impl UartIrq for MockIrq {
    fn handle(&mut self, rx: &mut dyn IrqRxSink) -> Option<SerialIrqEvent> {
        if let Some(sample) = self.sample.take() {
            rx.push(sample);
        }
        self.pending.take()
    }
}

struct MockSink {
    sample: Option<RxSample>,
}

impl IrqRxSink for MockSink {
    fn push(&mut self, sample: RxSample) {
        self.sample = Some(sample);
    }
}

struct MockUart;

impl SplitUart for MockUart {
    type Port = MockPort;
    type Irq = MockIrq;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "mock-uart",
            register_base: 0x1000,
            initial_baudrate: 115_200,
        }
    }

    fn split(self) -> UartParts<Self::Port, Self::Irq> {
        UartParts::new(
            MockPort::new(),
            MockIrq {
                pending: Some(SerialIrqEvent {
                    events: SerialEventSet::RX_DATA,
                    rx_errors: RxErrorFlags::empty(),
                    rearm: SerialEventSet::RX_DATA,
                }),
                sample: Some(RxSample {
                    byte: Some(b'a'),
                    flag: RxFlag::Normal,
                    overrun: false,
                }),
            },
        )
    }
}

#[axtest]
fn split_uart_exposes_independent_task_and_irq_endpoints() {
    let uart = MockUart;
    let info = uart.runtime_info();
    ax_assert_eq!(info.name, "mock-uart");
    ax_assert_eq!(info.register_base, 0x1000);
    ax_assert_eq!(info.initial_baudrate, 115_200);

    let UartParts { mut port, mut irq } = uart.split();
    let config = Config::new().baudrate(57_600);
    ax_assert!(port.startup(&config).is_ok());
    ax_assert_eq!(port.config.baudrate, Some(57_600));
    ax_assert_eq!(port.write_tx(b"abcdef"), 4);
    ax_assert!(!port.tx_idle());
    ax_assert_eq!(
        port.rearm(SerialEventSet::RX_DATA | SerialEventSet::TX_SPACE),
        SerialEventSet::RX_DATA
    );

    let mut sink = MockSink { sample: None };
    let event = irq.handle(&mut sink).expect("mock IRQ has pending event");
    ax_assert_eq!(sink.sample.expect("IRQ pushed sample").byte, Some(b'a'));
    ax_assert_eq!(event.events, SerialEventSet::RX_DATA);
}
