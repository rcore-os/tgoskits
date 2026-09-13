//! Deterministic handoff checks using the real worker and a backpressured UART.

use core::sync::atomic::AtomicUsize;

use super::{
    super::{log_mailbox::LogRecordMeta, *},
    *,
};

#[derive(Default)]
struct Wire {
    room: AtomicUsize,
    bytes: std::sync::Mutex<Vec<u8>>,
}

struct Port(Arc<Wire>);
impl rdif_serial::UartPort for Port {
    fn startup(&mut self, _: &Config) -> Result<(), rdif_serial::ConfigError> {
        Ok(())
    }
    fn shutdown(&mut self) {}
    fn set_config(&mut self, _: &Config) -> Result<(), rdif_serial::ConfigError> {
        Ok(())
    }
    fn read_rx(&mut self) -> Option<RxSample> {
        None
    }
    fn discard_rx(&mut self) {}
    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        let n = self.0.room.load(Ordering::Relaxed).min(bytes.len());
        self.0.room.fetch_sub(n, Ordering::Relaxed);
        self.0.bytes.lock().unwrap().extend_from_slice(&bytes[..n]);
        n
    }
    fn discard_tx(&mut self) -> bool {
        false
    }
    fn tx_idle(&mut self) -> bool {
        true
    }
    fn mask(&mut self, _: SerialEventSet) {}
    fn mask_all(&mut self) {}
    fn rearm(&mut self, _: SerialEventSet) -> SerialEventSet {
        SerialEventSet::empty()
    }
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
            paddr: 0,
            initial_baudrate: 115200,
            irq: None,
        },
        owner_cpu: 0,
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
        assert_eq!(wire.bytes.lock().unwrap().len(), initial_room);
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
        assert_eq!(wire.bytes.lock().unwrap().len(), initial_room);
        wire.room.store(4096, Ordering::Relaxed);
        let expected = b"old-record\r\nmailbox-record\r\nnew-log\r\nguest-bytes\nhost-bytes\n";
        while wire.bytes.lock().unwrap().len() < expected.len() {
            let before = wire.bytes.lock().unwrap().len();
            assert!(!worker.service_tx().blocked);
            assert!(
                wire.bytes.lock().unwrap().len() > before,
                "UART must make progress"
            );
        }
        let sent = wire.bytes.lock().unwrap();
        assert_eq!(
            sent.as_slice(),
            expected,
            "initial UART write room: {initial_room}"
        );
    }
}
