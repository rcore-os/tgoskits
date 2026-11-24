//! Deterministic handoff checks using the real worker and a backpressured UART.

use alloc::collections::VecDeque;
use core::sync::atomic::AtomicUsize;

use super::{super::*, *};

// Model separate early output and architecture input backends on the host.
// Bare-metal tests continue to query the platform's actual console ownership.
#[cfg(not(axtest))]
pub(super) fn early_console_uses_io_port(port: u16) -> bool {
    port == 0x2f8
}

#[cfg(not(axtest))]
pub(super) fn early_console_reserves_io_port(port: u16) -> bool {
    matches!(port, 0x3f8 | 0x2f8)
}

#[derive(Default)]
struct Wire {
    room: AtomicUsize,
    busy: AtomicBool,
    bytes: RawSpinLock<Vec<u8>>,
    rx: RawSpinLock<VecDeque<RxSample>>,
}

struct Port(Arc<Wire>);
impl rdif_serial::UartPort for Port {
    fn startup(&mut self, _: &Config) -> Result<(), rdif_serial::ConfigError> {
        self.discard_rx();
        Ok(())
    }
    fn startup_preserving_fifos(&mut self, _: &Config) -> Result<(), rdif_serial::ConfigError> {
        Ok(())
    }
    fn shutdown(&mut self) {}
    fn set_config(&mut self, _: &Config) -> Result<(), rdif_serial::ConfigError> {
        Ok(())
    }
    fn read_rx(&mut self) -> Option<RxSample> {
        self.0.rx.lock_irqsave().pop_front()
    }
    fn discard_rx(&mut self) {
        self.0.rx.lock_irqsave().clear();
    }
    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        let n = self.0.room.load(Ordering::Relaxed).min(bytes.len());
        self.0.room.fetch_sub(n, Ordering::Relaxed);
        self.0.bytes.lock_irqsave().extend_from_slice(&bytes[..n]);
        n
    }
    fn discard_tx(&mut self) -> bool {
        false
    }
    fn tx_idle(&mut self) -> bool {
        !self.0.busy.load(Ordering::Relaxed)
    }
    fn mask(&mut self, _: SerialEventSet) {}
    fn mask_all(&mut self) {}
    fn rearm(&mut self, _: SerialEventSet) -> SerialEventSet {
        SerialEventSet::empty()
    }
}

#[test]
fn queued_irq_input_precedes_direct_port_input_across_backpressure() {
    let (runtime, mut worker, wire) = fixture();
    let mut input = runtime
        .shared
        .rx_subscription
        .lock_irqsave()
        .take()
        .unwrap();
    let (producer, consumer) = spsc::channel(4);
    worker.irq_rx = consumer;
    let mut irq = RuntimeIrqPublisher {
        producer,
        bridge: runtime.shared.bridge.clone(),
        stats: runtime.shared.stats.clone(),
    };
    let sample = |byte| RxSample {
        byte: Some(byte),
        ..RxSample::default()
    };
    let mut report = rdif_serial::SerialIrqReport::default();
    report.rx.try_push(sample(b'v')).unwrap();
    irq.publish(report);
    wire.rx.lock_irqsave().extend(b"irtual".map(sample));

    // A port-read decision can precede an IRQ which consumes the first byte.
    // The actual worker must merge that older IRQ prefix before direct reads,
    // retaining the same order when the subscriber's ring fills mid-stream.
    let mut received = Vec::new();
    for _ in 0..2 {
        worker.service_rx(RxPath::Port);
        let mut chunk = [RxItem::default(); 4];
        let count = input.drain(&mut chunk);
        for item in &chunk[..count] {
            match item {
                RxItem::Byte { byte, .. } => received.push(*byte),
                RxItem::Overrun => panic!("the bounded input must not overrun"),
            }
        }
    }
    assert_eq!(received, b"virtual");
    assert!(worker.pending_rx.is_none());
    assert!(worker.irq_rx.is_empty());
    assert!(wire.rx.lock_irqsave().is_empty());
}
struct Emergency;
impl rdif_serial::UartEmergencyTx for Emergency {
    unsafe fn mask_interrupts_unlocked(&self) {
        panic!("unexpected emergency access")
    }
    unsafe fn try_write_unlocked(&self, _: &[u8]) -> usize {
        panic!("unexpected emergency access")
    }
}

fn fixture() -> (SerialRuntimeHandle, SerialWorker, Arc<Wire>) {
    fixture_with_address(SerialRegisterAddress::Mmio(0))
}

fn fixture_with_address(
    address: SerialRegisterAddress,
) -> (SerialRuntimeHandle, SerialWorker, Arc<Wire>) {
    let wire = Arc::new(Wire::default());
    let mailbox = Arc::new(LogMailbox::new(1));
    assert!(mailbox.claim(0));
    let (_, irq_rx) = spsc::channel(4);
    let (rx_output, rx_input) = spsc::channel(4);
    let shared = Arc::new(RuntimeShared {
        index: 0,
        info: SerialDeviceInfo {
            name: "probe".into(),
            device_id: rdrive::DeviceId::new(),
            firmware_path: "probe".into(),
            alias_index: None,
            address,
            initial_baudrate: 115200,
            irq: None,
        },
        initial_line: OnceLock::new(),
        owner_cpu: 0,
        #[cfg(feature = "fault-injection")]
        deferred_test_consumed: AtomicU8::new(0),
        polling: false,
        port: RawSpinLock::new(Box::new(Port(wire.clone()))),
        register_gate: Arc::new(UartRegisterGate::new(Emergency)),
        ingress: TxIngress::new(),
        log_mailbox: mailbox,
        rx_subscription: RawSpinLock::new(Some(rx_input)),
        log_subscription_gate: RawSpinLock::new(OrderedOutput::new(8)),
        log_subscription_active: AtomicBool::new(false),
        log_subscription_dropped_records: AtomicUsize::new(0),
        log_subscription_dropped_bytes: AtomicUsize::new(0),
        control: ControlQueue::new(),
        bridge: Arc::new(RuntimeIrqBridge::new()),
        stats: Arc::new(SerialStatsAtomic::new()),
        rx_source: Arc::new(PollSet::new()),
        tx_source: Arc::new(PollSet::new()),
        rx_progress: WaitQueue::new(),
        console_progress: WaitQueue::new(),
        tx_progress: WaitQueue::new(),
        tty_output_lock: Mutex::new(()),
        log_barriers: AtomicUsize::new(0),
        lifecycle: RuntimeLifecycle::new(),
        irq_handle: OnceLock::new(),
    });
    shared.set_started(true);
    shared.ingress.start_accepting();
    let worker = SerialWorker::new(shared.clone(), irq_rx, rx_output);
    (SerialRuntimeHandle { shared }, worker, wire)
}

#[cfg(not(axtest))]
#[test]
fn pio_handoff_preserves_input_from_either_early_backend() {
    for (port, keeps_input) in [(0x3f8, true), (0x2f8, true), (0x3e8, false)] {
        let (runtime, mut worker, wire) = fixture_with_address(SerialRegisterAddress::IoPort(port));
        runtime.shared.set_started(false);
        let mut input = runtime
            .shared
            .rx_subscription
            .lock_irqsave()
            .take()
            .unwrap();
        wire.rx.lock_irqsave().push_back(RxSample {
            byte: Some(b'k'),
            ..RxSample::default()
        });

        worker.start_handoff_console().unwrap();
        worker.service_rx(RxPath::Port);

        let mut received = [RxItem::default(); 1];
        assert_eq!(
            input.drain(&mut received),
            usize::from(keeps_input),
            "port {port:#x}"
        );
        if keeps_input {
            assert!(matches!(received[0], RxItem::Byte { byte: b'k', .. }));
        }
    }
}

#[test]
fn software_tx_drain_completes_while_uart_is_busy() {
    let (runtime, mut worker, wire) = fixture();
    let sender = SerialTxSender {
        shared: runtime.shared.clone(),
    };
    assert_eq!(sender.try_write(b"out").unwrap(), 3);
    wire.busy.store(true, Ordering::Relaxed);
    assert_eq!(worker.tx_drain_result(false), None);

    wire.room.store(3, Ordering::Relaxed);
    worker.service_tx();
    assert_eq!(worker.tx_drain_result(false), Some(Ok(())));
    assert_eq!(worker.tx_drain_result(true), None);

    wire.busy.store(false, Ordering::Relaxed);
    assert_eq!(worker.tx_drain_result(true), Some(Ok(())));
}

#[cfg(axtest)]
pub(in crate::serial) fn input_arriving_during_uart_drain_survives_flush() -> bool {
    let (runtime, worker, wire) = fixture();
    let output = runtime.task_output();
    let input = runtime.take_rx_subscription().unwrap();
    let sample = |byte| RxSample {
        byte: Some(byte),
        ..RxSample::default()
    };
    assert_eq!(output.try_write(b"out").unwrap(), 3);
    wire.busy.store(true, Ordering::Relaxed);
    wire.rx.lock_irqsave().push_back(sample(b'o'));

    let worker = Arc::new(Mutex::new(worker));
    let worker_ref = Arc::downgrade(&worker);
    let device = wire.clone();
    runtime.shared.control.set_test_service(move || {
        let worker = worker_ref.upgrade().unwrap();
        let mut worker = worker.lock();
        worker.process_control_commands();
        if worker.pending_drain.is_some() {
            device.room.store(3, Ordering::Relaxed);
            worker.service_tx();
            if worker.progress_tx_drain() {
                // Receive a new byte while the last character is still on the wire.
                device.rx.lock_irqsave().push_back(sample(b'n'));
                device.busy.store(false, Ordering::Relaxed);
                worker.progress_tx_drain();
            }
        }
        assert!(!worker.control_transaction_pending());
    });

    output
        .reconfigure_with(
            None,
            true,
            || {
                assert_eq!(wire.bytes.lock_irqsave().as_slice(), b"out");
                input.discard_pending()
            },
            || {},
        )
        .unwrap();

    worker.lock().service_rx(RxPath::Port);
    let mut received = [RxItem::default(); 4];
    input.drain(&mut received) == 1 && matches!(received[0], RxItem::Byte { byte: b'n', .. })
}

#[test]
fn discard_tx_clears_software_output_without_hardware_flush() {
    let (runtime, mut worker, wire) = fixture();
    let sender = SerialTxSender {
        shared: runtime.shared.clone(),
    };
    let sample = RxSample {
        byte: Some(b'r'),
        ..RxSample::default()
    };
    wire.rx.lock_irqsave().push_back(sample);

    assert_eq!(sender.try_write(b"buffered").unwrap(), 8);
    assert!(worker.service_tx().blocked);
    assert_eq!(sender.try_write(b"queued").unwrap(), 6);

    worker.discard_tx().unwrap();

    wire.room.store(64, Ordering::Relaxed);
    assert_eq!(sender.try_write(b"fresh").unwrap(), 5);
    assert!(!worker.service_tx().blocked);
    assert_eq!(wire.bytes.lock_irqsave().as_slice(), b"fresh");
    assert_eq!(
        runtime.shared.with_port(|port| port.read_rx()),
        Some(Some(sample))
    );
}

#[test]
fn pending_log_precedes_new_output_after_subscription_handoff() {
    for initial_room in [0, 3] {
        let (runtime, mut worker, wire) = fixture();
        let meta = LogRecordMeta::print(0, None);
        assert!(
            runtime
                .shared
                .publish_log(0, meta, format_args!("old-record\n"))
                .published()
        );
        wire.room.store(initial_room, Ordering::Relaxed);
        assert!(worker.service_tx().blocked);
        assert!(worker.pending_log.is_some());
        assert_eq!(wire.bytes.lock_irqsave().len(), initial_room);
        assert!(
            runtime
                .shared
                .publish_log(0, meta, format_args!("mailbox-record\n"))
                .published()
        );
        let subscription = runtime.take_log_subscription().unwrap();
        assert!(
            runtime
                .shared
                .publish_log(0, meta, format_args!("new-log\n"))
                .published()
        );
        subscription.write_output(1, b"guest-bytes\n").unwrap();
        // Exercise the actual ingress used by the physical output task.
        let sender = SerialTxSender {
            shared: runtime.shared.clone(),
        };
        while let Some(record) = subscription.try_read() {
            assert_eq!(
                sender.try_write(record.bytes()).unwrap(),
                record.bytes().len()
            );
        }
        assert_eq!(sender.try_write(b"host-bytes\n").unwrap(), 11);
        assert!(worker.service_tx().blocked);
        assert_eq!(wire.bytes.lock_irqsave().len(), initial_room);
        wire.room.store(4096, Ordering::Relaxed);
        let expected = b"old-record\r\nmailbox-record\r\nnew-log\r\nguest-bytes\nhost-bytes\n";
        while wire.bytes.lock_irqsave().len() < expected.len() {
            let before = wire.bytes.lock_irqsave().len();
            assert!(!worker.service_tx().blocked);
            assert!(
                wire.bytes.lock_irqsave().len() > before,
                "UART must make progress"
            );
        }
        let sent = wire.bytes.lock_irqsave();
        assert_eq!(
            sent.as_slice(),
            expected,
            "initial UART write room: {initial_room}"
        );
    }
}

#[test]
fn console_event_wait_rejects_stopped_and_foreign_subscriptions() {
    let (runtime, _worker, _wire) = fixture();
    let input = runtime.take_rx_subscription().unwrap();
    let logs = runtime.take_log_subscription().unwrap();
    let (other, _other_worker, _other_wire) = fixture();
    let foreign_logs = other.take_log_subscription().unwrap();
    assert_eq!(
        input.wait_console_event_until(Some(&foreign_logs), || false),
        Err(RuntimeError::OperationNotSupported),
    );
    runtime.shared.set_started(false);
    assert_eq!(
        input.wait_console_event_until(Some(&logs), || false),
        Err(RuntimeError::SerialNotStarted),
    );
    assert_eq!(
        logs.wait_readable_until(|| false),
        Err(RuntimeError::SerialNotStarted),
    );
}
