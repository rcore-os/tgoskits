use alloc::{boxed::Box, vec};
use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull, sync::atomic::AtomicBool};
use std::sync::Mutex;

use rd_net::{
    FixedNetControl, IRxQueue, ITxQueue, NetDevice, NetDeviceInfo, NetDeviceParts,
    NetHardIrqEndpoint, NetHardIrqHandler, NetHardIrqResult, NetIrqSourceId, NetOwnerStartup,
    NetOwnerStartupProgress, NetPollGroupId, NetPollGroupParts, NetPollIrqControl, NetQueueId,
    NetQueuePairParts, QueueConfig, SubmitError, TxNotify,
    dma_api::{
        DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
        DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    },
};

use super::*;
use crate::queue_runtime::{
    NetQueueIdentity, install_queue_backpressure_observer, install_queue_poll_observer,
    install_queue_rearm_observer, install_rx_publish_observer, install_tx_submit_observer,
    publish_queue_backpressure_gate, publish_queue_poll_gate, publish_queue_rearm_gate,
    publish_rx_publish_gate, publish_tx_submit_gate, spsc_ring, tests::TEST_DMA,
};

type Trace = Arc<Mutex<Vec<&'static str>>>;

fn test_identity() -> NetQueueIdentity {
    NetQueueIdentity {
        discovery_order: 0,
        group_id: NetPollGroupId::new(0),
        owner_cpu: 0,
    }
}

struct FailingDma(AtomicBool);

impl DmaOp for FailingDma {
    fn page_size(&self) -> usize {
        TEST_DMA.page_size()
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        if self.0.load(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: forward the caller's allocation contract to the host allocator.
        unsafe { TEST_DMA.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: every successful allocation came from TEST_DMA.
        unsafe { TEST_DMA.dealloc_contiguous(handle) }
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        if self.0.load(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: forward the caller's allocation contract to the host allocator.
        unsafe { TEST_DMA.alloc_coherent(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        // SAFETY: every successful allocation came from TEST_DMA.
        unsafe { TEST_DMA.dealloc_coherent(handle) }
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        // SAFETY: preserve the caller's live buffer, size and direction.
        unsafe { TEST_DMA.map_streaming(constraints, addr, size, direction) }
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        // SAFETY: this handle was created by the delegated map_streaming.
        unsafe { TEST_DMA.unmap_streaming(handle) }
    }
}

#[test]
fn rx_allocation_failure_recovers_without_disabling_tx() {
    static DMA: FailingDma = FailingDma(AtomicBool::new(false));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &DMA,
    );
    let mut device = rd_net::prepare_device(
        Box::new(TestDevice(Arc::clone(&trace), TestTx(Arc::clone(&trace)))),
        dma,
    )
    .unwrap();
    let mut group = device.poll_groups.pop().unwrap();
    group.rx.initial_refill(2).unwrap();
    // Hold the third preallocated pool token so the next replacement reaches
    // the allocator. Only this device's allocator can fail in this test.
    let spare = group.rx.allocate_replacement().unwrap();
    let (rx_ready, mut received) = spsc_ring(2);
    let (recycle, rx_recycle) = spsc_ring(2);
    let (mut transmit, tx_ready) = spsc_ring(2);
    let (tx_free, _free) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(
        test_identity(),
        Arc::new(QueueNotification::new()),
    ));
    shared.activate(false);
    let mut executor = QueueGroupExecutor {
        wifi_startup_group: None,
        group,
        rx_ready,
        rx_recycle,
        rx_recycler: Arc::new(RxRecycler::new(recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        rx_extra_buffers: 0,
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    };
    DMA.0.store(true, Ordering::Relaxed);
    let outcome = executor.poll(1);
    DMA.0.store(false, Ordering::Relaxed);
    assert!(
        !matches!(outcome, GroupPollOutcome::Failed(_)),
        "temporary DMA allocation failure permanently disabled RX and TX"
    );
    assert!(
        received.pop().is_none(),
        "packet escaped without a replacement"
    );

    drop(spare);
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                }
            })
            .is_ok()
    );
    assert!(matches!(executor.poll(256), GroupPollOutcome::More(_)));
    assert!(matches!(executor.poll(256), GroupPollOutcome::Idle(_)));
    let packet = received
        .pop()
        .expect("RX must resume after allocation recovers");
    packet
        .buffer
        .read_with_cpu(60, |bytes| assert_eq!(bytes, &[0; 60]));
    assert!(
        received.pop().is_none(),
        "the packet dropped under memory pressure was delivered"
    );
    assert_eq!(
        &*trace.lock().unwrap(),
        &["rx", "tx", "flush", "retry", "rx", "refill", "refill"]
    );
    assert!(executor.pending_rx_refill.is_empty());
    assert_eq!(executor.shared.take_pending_rx_drops(), 1);
    assert_eq!(executor.shared.take_pending_rx_drops(), 0);

    let limit = executor.group.rx.capacity().max(QUEUE_BUDGET);
    let mut held = Vec::new();
    while executor.rx_extra_buffers < limit {
        held.push(executor.take_rx_replacement().unwrap());
    }
    assert!(
        executor.take_rx_replacement().is_none(),
        "detached RX tokens exceeded the queue budget"
    );
    let recycled = held.pop().unwrap();
    let address = recycled.read_with_cpu(1, |bytes| bytes.as_ptr() as usize);
    executor.rx_recycler.recycle(recycled);
    executor
        .rx_recycler
        .drain_into(&mut executor.rx_recycle, &mut executor.rx_spares, 1);
    let reused = executor
        .take_rx_replacement()
        .expect("recycled tokens remain usable at the limit");
    assert_eq!(
        reused.read_with_cpu(1, |bytes| bytes.as_ptr() as usize),
        address
    );
    assert_eq!(executor.rx_extra_buffers, limit);
}

fn queue_config() -> QueueConfig {
    QueueConfig {
        ring_size: 3,
        buf_size: 2048,
        align: 64,
        dma_mask: u64::MAX,
    }
}

struct TestTx(Trace);

impl ITxQueue for TestTx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.0.lock().unwrap().push("tx");
        Ok(())
    }
    fn flush(&mut self) {
        self.0.lock().unwrap().push("flush");
    }
    fn reclaim(&mut self) -> Option<DmaBuffer> {
        None
    }
}

struct TestRx {
    trace: Trace,
    completions: VecDeque<RxCompletion>,
    initial: usize,
    reclaimed: usize,
    replacements: Vec<DmaBuffer>,
}

impl IRxQueue for TestRx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, mut buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.initial > 0 {
            self.initial -= 1;
            buffer.write_with_cpu(|packet| packet.fill(self.initial as u8));
            self.completions.push_back(RxCompletion {
                buffer,
                packet_len: 60,
            });
            return Ok(());
        }
        // Model a software queue whose owner cannot accept more buffers
        // until both completion slots have been consumed.
        if self.reclaimed < 2 {
            self.trace.lock().unwrap().push("retry");
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        self.trace.lock().unwrap().push("refill");
        self.replacements.push(buffer);
        Ok(())
    }
    fn reclaim(&mut self) -> Option<RxCompletion> {
        let completion = self.completions.pop_front()?;
        self.reclaimed += 1;
        self.trace.lock().unwrap().push("rx");
        Some(completion)
    }
}

struct TestIrq;
impl NetHardIrqHandler for TestIrq {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        NetHardIrqResult::Spurious
    }
}

struct MissingDeviceStartup {
    cancel_attempted: Arc<AtomicBool>,
    cancel_fails: bool,
}

impl NetOwnerStartup for MissingDeviceStartup {
    fn start(&mut self, _now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError> {
        Err(NetError::DeviceNotPresent)
    }

    fn advance(&mut self, _now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError> {
        panic!("a missing device must not advance startup")
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        self.cancel_attempted.store(true, Ordering::Release);
        if self.cancel_fails {
            Err(NetError::InvalidParts)
        } else {
            Ok(())
        }
    }
}
impl NetPollIrqControl for TestIrq {
    fn quiesce(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        Ok(NetRearmResult::Idle)
    }
}

struct TestDevice<T>(Trace, T);
impl<T: ITxQueue> rd_net::DriverGeneric for TestDevice<T> {
    fn name(&self) -> &str {
        "test"
    }
}
impl<T: ITxQueue + 'static> NetDevice for TestDevice<T> {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("test", [0; 6]),
            control: Box::new(FixedNetControl::new([0; 6])),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(self.1),
                    rx: Box::new(TestRx {
                        trace: self.0,
                        completions: VecDeque::new(),
                        initial: 2,
                        reclaimed: 0,
                        replacements: Vec::new(),
                    }),
                },
                irq_control: Box::new(TestIrq),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(TestIrq),
                )],
            }],
        })
    }
}

#[test]
fn missing_device_startup_is_cancelled_without_publishing_queues() {
    for cancel_fails in [false, true] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let dma = DeviceDma::new(
            DmaDeviceInfo::new(
                DmaDomainId::Direct,
                DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            &TEST_DMA,
        );
        let mut device =
            rd_net::prepare_device(Box::new(TestDevice(Arc::clone(&trace), TestTx(trace))), dma)
                .unwrap();
        let mut group = device.poll_groups.pop().unwrap();
        let cancel_attempted = Arc::new(AtomicBool::new(false));
        group.owner_startup = Some(Box::new(MissingDeviceStartup {
            cancel_attempted: Arc::clone(&cancel_attempted),
            cancel_fails,
        }));

        let (rx_ready, _protocol_rx) = spsc_ring(2);
        let (rx_recycle, recycle) = spsc_ring(2);
        let (tx_free, mut protocol_tx_free) = spsc_ring(2);
        let (_protocol_tx, tx_ready) = spsc_ring(2);
        let shared = Arc::new(PollGroupState::new(
            test_identity(),
            Arc::new(QueueNotification::new()),
        ));
        let mut executor = QueueGroupExecutor {
            wifi_startup_group: None,
            group,
            rx_ready,
            rx_recycle: recycle,
            rx_recycler: Arc::new(RxRecycler::new(rx_recycle, Arc::clone(&shared), 2)),
            rx_spares: Vec::new(),
            rx_extra_buffers: 0,
            tx_ready,
            tx_free,
            pending_rx: None,
            pending_rx_refill: VecDeque::with_capacity(2),
            pending_tx: None,
            pending_tx_free: None,
            retry_at: None,
            shared: Arc::clone(&shared),
        };

        let result = executor.initialize(|| panic!("absent device startup must not wait"));
        if cancel_fails {
            assert!(matches!(result, Err(NetError::InvalidParts)));
        } else {
            assert!(result.is_ok());
        }
        assert!(cancel_attempted.load(Ordering::Acquire));
        assert_eq!(shared.startup_absent(), !cancel_fails);
        assert!(shared.is_disabled());
        assert_eq!(executor.group.rx.posted(), 0);
        assert!(protocol_tx_free.pop().is_none());
    }
}

struct StartupWifi(Arc<AtomicBool>);

impl Drop for StartupWifi {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl rd_net::WifiControl for StartupWifi {
    fn start(
        &mut self,
        _operation: &rd_net::WifiOperation,
        _now_nanos: u64,
    ) -> Result<rd_net::WifiControlProgress, NetError> {
        panic!("Wi-Fi transactions must not start before publication")
    }

    fn advance(&mut self, _now_nanos: u64) -> Result<rd_net::WifiControlProgress, NetError> {
        panic!("Wi-Fi transactions must not advance before publication")
    }

    fn cancel(&mut self) -> Result<(), NetError> {
        panic!("no Wi-Fi transaction is active before publication")
    }

    fn startup_transaction(&self) -> Option<rd_net::WifiTransaction> {
        None
    }
}

fn startup_executor(absent: bool, trace: Trace) -> QueueGroupExecutor {
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let mut device =
        rd_net::prepare_device(Box::new(TestDevice(Arc::clone(&trace), TestTx(trace))), dma)
            .unwrap();
    let group = device.poll_groups.pop().unwrap();
    let (rx_ready, _protocol_rx) = spsc_ring(2);
    let (rx_recycle, recycle) = spsc_ring(2);
    let (tx_free, _protocol_tx_free) = spsc_ring(2);
    let (_protocol_tx, tx_ready) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(
        test_identity(),
        Arc::new(QueueNotification::new()),
    ));
    let mut executor = QueueGroupExecutor {
        group,
        wifi_startup_group: None,
        rx_ready,
        rx_recycle: recycle,
        rx_recycler: Arc::new(RxRecycler::new(rx_recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        rx_extra_buffers: 0,
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    };
    if absent {
        executor.group.owner_startup = Some(Box::new(MissingDeviceStartup {
            cancel_attempted: Arc::new(AtomicBool::new(false)),
            cancel_fails: false,
        }));
        executor
            .initialize(|| panic!("absent startup must not wait"))
            .unwrap();
    }
    executor
}

#[test]
fn startup_pruning_releases_absent_queues_and_remaps_wifi_slots() {
    for absent in [
        vec![true, false, true, false],
        vec![true],
        vec![false],
        vec![],
    ] {
        let mut groups = Vec::new();
        let mut wifi = Vec::new();
        let mut queue_owners = Vec::new();
        let mut wifi_dropped = Vec::new();
        let mut states = Vec::new();
        for (group_index, &missing) in absent.iter().enumerate() {
            let trace = Arc::new(Mutex::new(Vec::new()));
            queue_owners.push(Arc::downgrade(&trace));
            let executor = startup_executor(missing, trace);
            states.push(Arc::clone(&executor.shared));
            groups.push(executor);
            let dropped = Arc::new(AtomicBool::new(false));
            wifi_dropped.push(Arc::clone(&dropped));
            wifi.push(WifiExecutorSlot {
                group_index,
                control: Box::new(StartupWifi(dropped)),
                queue: Arc::new(crate::queue_runtime::WifiControlQueue::new()),
                active: None,
            });
        }

        for command in [
            crate::queue_runtime::COMMAND_START,
            COMMAND_QUARANTINE,
            COMMAND_STOP,
        ] {
            assert_eq!(
                retain_started_executor_groups(&mut groups, &mut wifi, command),
                None
            );
            assert!(queue_owners.iter().all(|owner| owner.upgrade().is_some()));
            assert!(
                wifi_dropped
                    .iter()
                    .all(|dropped| !dropped.load(Ordering::Acquire))
            );
        }
        let status = retain_started_executor_groups(&mut groups, &mut wifi, COMMAND_RUN);

        let mut published = 0;
        for (index, &missing) in absent.iter().enumerate() {
            assert_eq!(queue_owners[index].upgrade().is_none(), missing);
            assert_eq!(wifi_dropped[index].load(Ordering::Acquire), missing);
            if !missing {
                assert_eq!(wifi[published].group_index, published);
                assert!(Arc::ptr_eq(&groups[published].shared, &states[index]));
                published += 1;
            }
        }
        assert_eq!(groups.len(), published);
        assert_eq!(wifi.len(), published);
        assert_eq!(
            status,
            Some(if published == 0 {
                STATUS_EMPTY
            } else {
                STATUS_READY
            })
        );
    }
}

#[test]
fn absent_wifi_control_stops_surviving_device_group() {
    let control = startup_executor(true, Arc::new(Mutex::new(Vec::new())));
    let mut sibling = startup_executor(false, Arc::new(Mutex::new(Vec::new())));
    sibling
        .initialize(|| panic!("sibling startup must not wait"))
        .unwrap();
    assert!(!sibling.shared.is_disabled());
    sibling.wifi_startup_group = Some(Arc::clone(&control.shared));
    sibling.stop_if_wifi_absent().unwrap();
    assert!(sibling.shared.startup_absent());
    assert!(sibling.shared.is_disabled());
    let (mut port, ..) = crate::queue_runtime::tests::tx_test_port(TxQueueDiscipline::NoQueue, 0);
    let (mut sibling_port, ..) =
        crate::queue_runtime::tests::tx_test_port(TxQueueDiscipline::NoQueue, 0);
    port.groups[0].shared = Arc::clone(&control.shared);
    sibling_port.groups[0].shared = Arc::clone(&sibling.shared);
    port.groups.append(&mut sibling_port.groups);
    let (ports, indices) = crate::queue_runtime::retain_started_ports(vec![port]);
    assert!(ports.is_empty());
    assert_eq!(indices, vec![None]);
    let dropped = Arc::new(AtomicBool::new(false));
    let mut wifi = vec![WifiExecutorSlot {
        group_index: 0,
        control: Box::new(StartupWifi(Arc::clone(&dropped))),
        queue: Arc::new(crate::queue_runtime::WifiControlQueue::new()),
        active: None,
    }];
    let mut groups = vec![control, sibling];
    assert_eq!(
        retain_started_executor_groups(&mut groups, &mut wifi, COMMAND_RUN),
        Some(STATUS_EMPTY)
    );
    assert!(groups.is_empty());
    assert!(wifi.is_empty());
    assert!(dropped.load(Ordering::Acquire));
}

struct PruneIrq {
    trace: Trace,
    fail_shutdown: bool,
}

impl NetPollIrqControl for PruneIrq {
    fn quiesce(&mut self) -> Result<(), NetError> {
        self.trace.lock().unwrap().push("quiesce");
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), NetError> {
        self.trace.lock().unwrap().push("shutdown");
        if self.fail_shutdown {
            Err(NetError::InvalidParts)
        } else {
            Ok(())
        }
    }

    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        Ok(NetRearmResult::Idle)
    }
}

#[test]
fn wifi_device_pruning_requires_shutdown_and_preserves_unrelated_groups() {
    for missing in [false, true] {
        for fail_shutdown in [false, true] {
            let control = startup_executor(missing, Arc::new(Mutex::new(Vec::new())));
            let trace = Arc::new(Mutex::new(Vec::new()));
            let mut sibling = startup_executor(false, Arc::new(Mutex::new(Vec::new())));
            sibling
                .initialize(|| panic!("sibling startup must not wait"))
                .unwrap();
            sibling.wifi_startup_group = Some(Arc::clone(&control.shared));
            sibling.group.irq_control = Box::new(PruneIrq {
                trace: Arc::clone(&trace),
                fail_shutdown,
            });
            assert_eq!(
                sibling.stop_if_wifi_absent().is_err(),
                missing && fail_shutdown
            );
            assert_eq!(sibling.shared.startup_absent(), missing && !fail_shutdown);
            assert_eq!(
                *trace.lock().unwrap(),
                if missing {
                    vec!["quiesce", "shutdown"]
                } else {
                    vec![]
                }
            );
            if missing && !fail_shutdown {
                sibling.stop_if_wifi_absent().unwrap();
                assert_eq!(*trace.lock().unwrap(), vec!["quiesce", "shutdown"]);
            }
            let mut unrelated = startup_executor(false, Arc::new(Mutex::new(Vec::new())));
            unrelated.stop_if_wifi_absent().unwrap();
            assert!(!unrelated.shared.startup_absent());
        }
    }
}

#[test]
fn rx_refill_retry_drains_completions_and_preserves_tx_flush() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let mut device = rd_net::prepare_device(
        Box::new(TestDevice(Arc::clone(&trace), TestTx(Arc::clone(&trace)))),
        dma,
    )
    .unwrap();
    let mut group = device.poll_groups.pop().unwrap();
    group.rx.initial_refill(2).unwrap();
    let (rx_ready, mut received) = spsc_ring(1);
    let (recycle, rx_recycle) = spsc_ring(2);
    let (mut transmit, tx_ready) = spsc_ring(2);
    let (tx_free, _free) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(
        test_identity(),
        Arc::new(QueueNotification::new()),
    ));
    shared.activate(false);
    let buffer = group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let mut executor = QueueGroupExecutor {
        group,
        wifi_startup_group: None,
        rx_ready,
        rx_recycle,
        rx_recycler: Arc::new(RxRecycler::new(recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        rx_extra_buffers: 0,
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    };

    assert!(matches!(executor.poll(2), GroupPollOutcome::More(2)));
    assert!(matches!(executor.poll(256), GroupPollOutcome::More(_)));
    assert_eq!(
        &*trace.lock().unwrap(),
        &["tx", "flush", "rx", "retry", "rx"]
    );
    assert!(
        received.pop().is_none(),
        "RX escaped before replacement was accepted"
    );
    assert_eq!(executor.pending_rx_refill.len(), 2);

    assert!(matches!(executor.poll(256), GroupPollOutcome::Blocked(_)));
    let first = received.pop().unwrap();
    first
        .buffer
        .read_with_cpu(60, |packet| assert_eq!(packet, &[1; 60]));
    assert!(executor.pending_rx.is_some());
    assert!(matches!(executor.poll(256), GroupPollOutcome::Idle(_)));
    let second = received.pop().unwrap();
    second
        .buffer
        .read_with_cpu(60, |packet| assert_eq!(packet, &[0; 60]));
    assert!(executor.pending_rx.is_none());
    assert!(executor.pending_rx_refill.is_empty());
    assert_eq!(
        trace
            .lock()
            .unwrap()
            .iter()
            .filter(|&&event| event == "refill")
            .count(),
        2
    );
}

struct GatedTx {
    blocked: Arc<AtomicBool>,
    packets: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl ITxQueue for GatedTx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }

    fn config(&self) -> QueueConfig {
        queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.blocked.load(Ordering::Relaxed) {
            return Err(SubmitError::new(buffer, NetError::Retry));
        }
        buffer.read_with_cpu(buffer.len(), |packet| {
            self.packets.lock().unwrap().push(packet.to_vec());
        });
        Ok(())
    }

    fn reclaim(&mut self) -> Option<DmaBuffer> {
        None
    }
}

#[test]
fn tx_backpressure_allows_rx_delivery_before_tx_resumes() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let blocked = Arc::new(AtomicBool::new(true));
    let packets = Arc::new(Mutex::new(Vec::new()));
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let mut device = rd_net::prepare_device(
        Box::new(TestDevice(
            trace,
            GatedTx {
                blocked: Arc::clone(&blocked),
                packets: Arc::clone(&packets),
            },
        )),
        dma,
    )
    .unwrap();
    let mut group = device.poll_groups.pop().unwrap();
    group.rx.initial_refill(2).unwrap();
    let (rx_ready, mut received) = spsc_ring(2);
    let (recycle, rx_recycle) = spsc_ring(2);
    let (mut transmit, tx_ready) = spsc_ring(2);
    let (tx_free, _free) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(
        test_identity(),
        Arc::new(QueueNotification::new()),
    ));
    shared.activate(false);
    for byte in [0xa5, 0x5a] {
        let mut buffer = group.tx_pool.allocate(60).unwrap();
        buffer.write_with_cpu(|packet| packet.fill(byte));
        assert!(
            transmit
                .push(TxRequest {
                    buffer,
                    options: TxSubmitOptions::default(),
                })
                .is_ok()
        );
    }
    let mut executor = QueueGroupExecutor {
        group,
        wifi_startup_group: None,
        rx_ready,
        rx_recycle,
        rx_recycler: Arc::new(RxRecycler::new(recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        rx_extra_buffers: 0,
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    };
    // A software-backed NIC may need its completed RX slots drained before
    // the common owner can finish outstanding TX. Keep TX blocked until RX
    // delivery is proven, rather than relying on an IRQ or a timed retry.
    executor.poll(256);
    executor.poll(256);
    for byte in [1, 0] {
        let completion = received
            .pop()
            .expect("TX Retry starved a completed RX packet");
        completion
            .buffer
            .read_with_cpu(60, |packet| assert_eq!(packet, &[byte; 60]));
    }
    assert!(packets.lock().unwrap().is_empty());
    assert!(
        matches!(executor.poll(256), GroupPollOutcome::Idle(_)),
        "a still-blocked TX must rearm instead of busy-polling"
    );
    blocked.store(false, Ordering::Relaxed);
    executor.poll(256);
    executor.poll(256);
    assert_eq!(
        *packets.lock().unwrap(),
        vec![vec![0xa5; 60], vec![0x5a; 60]]
    );
}

/// RX queue that rejects replacements with a non-retryable error, so a round
/// that already completed work can end in `Failed`.
struct HardFailingRx {
    trace: Trace,
    completions: VecDeque<RxCompletion>,
    initial: usize,
}

impl IRxQueue for HardFailingRx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, mut buffer: DmaBuffer) -> Result<(), SubmitError> {
        if self.initial > 0 {
            self.initial -= 1;
            buffer.write_with_cpu(|packet| packet.fill(self.initial as u8));
            self.completions.push_back(RxCompletion {
                buffer,
                packet_len: 60,
            });
            return Ok(());
        }
        Err(SubmitError::new(buffer, NetError::LinkDown))
    }
    fn reclaim(&mut self) -> Option<RxCompletion> {
        let completion = self.completions.pop_front()?;
        self.trace.lock().unwrap().push("rx");
        Some(completion)
    }
}

struct HardFailingRxDevice(Trace, usize);

impl rd_net::DriverGeneric for HardFailingRxDevice {
    fn name(&self) -> &str {
        "test-hard-failing-rx"
    }
}

impl NetDevice for HardFailingRxDevice {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("test-hard-failing-rx", [0; 6]),
            control: Box::new(FixedNetControl::new([0; 6])),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(TestTx(Arc::clone(&self.0))),
                    rx: Box::new(HardFailingRx {
                        trace: self.0,
                        completions: VecDeque::new(),
                        initial: self.1,
                    }),
                },
                irq_control: Box::new(TestIrq),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(TestIrq),
                )],
            }],
        })
    }
}

/// Device whose RX queue refuses replacement buffers with a retry until two
/// completions have been reclaimed, like a software queue that needs
/// completion-ring space before it accepts buffers.
struct RxRetryDevice(Trace);

impl rd_net::DriverGeneric for RxRetryDevice {
    fn name(&self) -> &str {
        "test-rx-retry"
    }
}

impl NetDevice for RxRetryDevice {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("test-rx-retry", [0; 6]),
            control: Box::new(FixedNetControl::new([0; 6])),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(TestTx(Arc::clone(&self.0))),
                    rx: Box::new(TestRx {
                        trace: self.0,
                        completions: VecDeque::new(),
                        initial: 0,
                        reclaimed: 0,
                        replacements: Vec::new(),
                    }),
                },
                irq_control: Box::new(TestIrq),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(TestIrq),
                )],
            }],
        })
    }
}

/// TX queue that refuses the next submission with a scripted outcome.
struct ScriptedTx {
    trace: Trace,
    refusal: Option<NetError>,
}

impl ITxQueue for ScriptedTx {
    fn id(&self) -> NetQueueId {
        NetQueueId::new(0)
    }
    fn config(&self) -> QueueConfig {
        queue_config()
    }
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        self.trace.lock().unwrap().push("tx");
        match self.refusal.take() {
            Some(reason) => Err(SubmitError::new(buffer, reason)),
            None => Ok(()),
        }
    }
    fn flush(&mut self) {
        self.trace.lock().unwrap().push("flush");
    }
    fn reclaim(&mut self) -> Option<DmaBuffer> {
        None
    }
}

/// IRQ control that replays one scripted rearm result, then reports idle.
struct ScriptedIrq {
    rearm: Option<NetRearmResult>,
    fail: bool,
}

impl NetHardIrqHandler for ScriptedIrq {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        NetHardIrqResult::Spurious
    }
}

impl NetPollIrqControl for ScriptedIrq {
    fn quiesce(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn shutdown(&mut self) -> Result<(), NetError> {
        Ok(())
    }
    fn rearm_and_check(&mut self, _now_nanos: u64) -> Result<NetRearmResult, NetError> {
        if self.fail {
            return Err(NetError::DeviceNotPresent);
        }
        Ok(self.rearm.take().unwrap_or(NetRearmResult::Idle))
    }
}

/// Device whose TX queue refuses the first submission with `refusal`.
struct ScriptedTxDevice {
    trace: Trace,
    refusal: NetError,
}

impl rd_net::DriverGeneric for ScriptedTxDevice {
    fn name(&self) -> &str {
        "test-scripted-tx"
    }
}

impl NetDevice for ScriptedTxDevice {
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError> {
        Ok(NetDeviceParts {
            info: NetDeviceInfo::new("test-scripted-tx", [0; 6]),
            control: Box::new(FixedNetControl::new([0; 6])),
            wifi_control: None,
            poll_groups: vec![NetPollGroupParts {
                id: NetPollGroupId::new(0),
                queues: NetQueuePairParts {
                    tx: Box::new(ScriptedTx {
                        trace: Arc::clone(&self.trace),
                        refusal: Some(self.refusal),
                    }),
                    rx: Box::new(TestRx {
                        trace: self.trace,
                        completions: VecDeque::new(),
                        initial: 0,
                        reclaimed: 0,
                        replacements: Vec::new(),
                    }),
                },
                irq_control: Box::new(ScriptedIrq {
                    rearm: None,
                    fail: false,
                }),
                owner_startup: None,
                irq_endpoints: vec![NetHardIrqEndpoint::new(
                    NetIrqSourceId::new(0),
                    Box::new(TestIrq),
                )],
            }],
        })
    }
}

fn port_test_executor(
    identity: NetQueueIdentity,
    trace: Trace,
    tx_ready: SpscConsumer<TxRequest>,
) -> QueueGroupExecutor {
    port_test_executor_with_rx_initial(identity, trace, tx_ready, 1)
}

/// Builds a port-test executor whose RX queue accepts `rx_initial`
/// replacements before it starts failing permanently, under its own identity
/// so its reports can be told apart from concurrent tests.
fn port_test_executor_with_rx_initial(
    identity: NetQueueIdentity,
    trace: Trace,
    tx_ready: SpscConsumer<TxRequest>,
    rx_initial: usize,
) -> QueueGroupExecutor {
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let device = rd_net::prepare_device(
        Box::new(HardFailingRxDevice(Arc::clone(&trace), rx_initial)),
        dma,
    )
    .unwrap();
    port_test_executor_from_device(identity, tx_ready, device)
}

/// Builds a port-test executor around an already prepared device.
fn port_test_executor_from_device(
    identity: NetQueueIdentity,
    tx_ready: SpscConsumer<TxRequest>,
    mut device: rd_net::PreparedNetDevice,
) -> QueueGroupExecutor {
    let mut group = device.poll_groups.pop().unwrap();
    // No initial refill and no queued completions: a plain round neither
    // reclaims nor refills, so its outcome depends only on the test's input.
    group.rx.initial_refill(0).unwrap();
    let (rx_ready, _protocol_rx) = spsc_ring(2);
    let (rx_recycle, recycle) = spsc_ring(2);
    let (tx_free, _protocol_tx_free) = spsc_ring(2);
    let shared = Arc::new(PollGroupState::new(
        identity,
        Arc::new(QueueNotification::new()),
    ));
    shared.activate(false);
    QueueGroupExecutor {
        group,
        wifi_startup_group: None,
        rx_ready,
        rx_recycle: recycle,
        rx_recycler: Arc::new(RxRecycler::new(rx_recycle, Arc::clone(&shared), 2)),
        rx_spares: Vec::new(),
        rx_extra_buffers: 0,
        tx_ready,
        tx_free,
        pending_rx: None,
        pending_rx_refill: VecDeque::with_capacity(2),
        pending_tx: None,
        pending_tx_free: None,
        retry_at: None,
        shared,
    }
}

// ---- queue poll observation port ----

/// The observation port is process-wide, so the tests below share one observer
/// function (installing the same function is idempotent, replacing a live one
/// is not), serialize on one lock, and filter captured reports by their own
/// identity: other tests in this binary keep polling while the gate is open.
struct PortCapture {
    polls: Vec<QueuePollReport>,
    rearms: Vec<QueueRearmReport>,
    backpressures: Vec<QueueBackpressureReport>,
    submits: Vec<TxSubmitReport>,
    publishes: Vec<RxPublishReport>,
}

impl PortCapture {
    const fn new() -> Self {
        Self {
            polls: Vec::new(),
            rearms: Vec::new(),
            backpressures: Vec::new(),
            submits: Vec::new(),
            publishes: Vec::new(),
        }
    }
}

static PORT_CAPTURE: Mutex<PortCapture> = Mutex::new(PortCapture::new());
static POLL_PORT_LOCK: Mutex<()> = Mutex::new(());

fn capture_poll_report(report: QueuePollReport) {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .polls
        .push(report);
}

fn capture_rearm_report(report: QueueRearmReport) {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .rearms
        .push(report);
}

fn capture_backpressure_report(report: QueueBackpressureReport) {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .backpressures
        .push(report);
}

fn capture_submit_report(report: TxSubmitReport) {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .submits
        .push(report);
}

fn capture_publish_report(report: RxPublishReport) {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .publishes
        .push(report);
}

fn install_port_capture() {
    install_queue_poll_observer(capture_poll_report);
    install_queue_rearm_observer(capture_rearm_report);
    install_queue_backpressure_observer(capture_backpressure_report);
    install_tx_submit_observer(capture_submit_report);
    install_rx_publish_observer(capture_publish_report);
}

/// Opens every port and clears the capture, so one test cannot observe another
/// test's reports or a previous round's leftovers.
fn begin_port_capture(enabled: bool) -> std::sync::MutexGuard<'static, ()> {
    let guard = POLL_PORT_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    install_port_capture();
    publish_queue_poll_gate(enabled);
    publish_queue_rearm_gate(enabled);
    publish_queue_backpressure_gate(enabled);
    publish_tx_submit_gate(enabled);
    publish_rx_publish_gate(enabled);
    let mut capture = PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    capture.polls.clear();
    capture.rearms.clear();
    capture.backpressures.clear();
    capture.submits.clear();
    capture.publishes.clear();
    drop(capture);
    guard
}

fn close_port_gates() {
    publish_queue_poll_gate(false);
    publish_queue_rearm_gate(false);
    publish_queue_backpressure_gate(false);
    publish_tx_submit_gate(false);
    publish_rx_publish_gate(false);
}

fn reports_of(identity: NetQueueIdentity) -> Vec<QueuePollReport> {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .polls
        .iter()
        .filter(|report| report.identity == identity)
        .copied()
        .collect()
}

fn rearms_of(identity: NetQueueIdentity) -> Vec<QueueRearmOutcome> {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .rearms
        .iter()
        .filter(|report| report.identity == identity)
        .map(|report| report.outcome)
        .collect()
}

fn backpressures_of(identity: NetQueueIdentity) -> Vec<QueueBackpressureReport> {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .backpressures
        .iter()
        .filter(|report| report.identity == identity)
        .copied()
        .collect()
}

fn submits_of(identity: NetQueueIdentity) -> Vec<TxSubmitReport> {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .submits
        .iter()
        .filter(|report| report.identity == identity)
        .copied()
        .collect()
}

fn publishes_of(identity: NetQueueIdentity) -> Vec<RxPublishReport> {
    PORT_CAPTURE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .publishes
        .iter()
        .filter(|report| report.identity == identity)
        .copied()
        .collect()
}

/// Identity reserved for one port test, so reports from other tests that poll
/// concurrently cannot be mistaken for its own.
fn port_identity(tag: usize) -> NetQueueIdentity {
    NetQueueIdentity {
        discovery_order: 900 + tag,
        group_id: NetPollGroupId::new(7),
        owner_cpu: 3,
    }
}

#[test]
fn queue_poll_reports_exactly_once_per_round_with_its_identity_and_budget() {
    let _port = begin_port_capture(true);
    let identity = port_identity(1);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    let outcome = executor.poll(256);
    assert_eq!(outcome, GroupPollOutcome::Idle(0));
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 1, "one round must report exactly once");
    assert_eq!(reports[0].budget, 256);
    assert_eq!(reports[0].work_units, 0);
    assert_eq!(reports[0].outcome, QueuePollOutcome::Idle);

    // A second round reports again, and still only once.
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let outcome = executor.poll(64);
    assert_eq!(outcome, GroupPollOutcome::Idle(1));
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[1].budget, 64);
    assert_eq!(reports[1].work_units, 1);

    // A budget of one unit leaves work behind, so the reported code is the one
    // that means "there is more to do".
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let outcome = executor.poll(1);
    assert_eq!(outcome, GroupPollOutcome::More(1));
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[2].budget, 1);
    assert_eq!(reports[2].work_units, 1);
    assert_eq!(reports[2].outcome, QueuePollOutcome::More);
    close_port_gates();
}

#[test]
fn queue_poll_reports_a_blocked_round() {
    let _port = begin_port_capture(true);
    let identity = port_identity(4);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    // Fill the protocol-facing RX ring and strand one more completion: the
    // round stops at the publish step instead of finishing.
    for _ in 0..2 {
        let buffer = executor.group.rx.allocate_replacement().unwrap();
        assert!(
            executor
                .rx_ready
                .push(RxCompletion {
                    buffer,
                    packet_len: 60,
                })
                .is_ok()
        );
    }
    let buffer = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx = Some(RxCompletion {
        buffer,
        packet_len: 60,
    });

    let outcome = executor.poll(256);
    assert_eq!(outcome, GroupPollOutcome::Blocked(0));
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].outcome, QueuePollOutcome::Blocked);
    assert_eq!(reports[0].work_units, 0);
    assert!(
        publishes_of(identity).is_empty(),
        "a refused publish must not be reported"
    );
    close_port_gates();
}

#[test]
fn rx_publish_reports_a_frame_retained_by_a_blocked_round() {
    let _port = begin_port_capture(true);
    let identity = port_identity(14);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);
    let buffer = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx = Some(RxCompletion {
        buffer,
        packet_len: 60,
    });

    let outcome = executor.poll(256);
    assert_eq!(outcome, GroupPollOutcome::Idle(0));
    let published = publishes_of(identity);
    assert_eq!(published.len(), 1, "the retained frame is published once");
    assert_eq!(published[0].len, 60);
    close_port_gates();
}

#[test]
fn queue_poll_reports_the_work_done_before_a_failed_round() {
    let _port = begin_port_capture(true);
    let identity = port_identity(2);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    // One submitted frame and one pending refill that the device rejects
    // permanently: the round does work and then fails.
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let replacement = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx_refill.push_back(PendingRxRefill {
        completion: None,
        replacement,
    });

    let outcome = executor.poll(256);
    assert!(
        matches!(outcome, GroupPollOutcome::Failed(_)),
        "a permanent RX refill error must fail the round"
    );
    // The round accepts one submitted frame, one replacement and one reclaimed
    // completion before the second replacement is rejected, so the expected
    // work is a constant of this setup rather than a read-back of the value
    // under test.
    let expected_work = 3;
    assert_eq!(
        outcome.work(),
        expected_work,
        "one TX submit, one accepted replacement and one reclaimed frame"
    );
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].outcome, QueuePollOutcome::Failed);
    assert_eq!(
        reports[0].work_units, expected_work,
        "a failed round must report the work it completed"
    );
    assert!(
        executor.shared.is_disabled(),
        "a failed round must leave the group disabled"
    );
    close_port_gates();
}

/// Queues one frame and polls once, so a compared round moves a frame
/// instead of returning an empty `Idle(0)`.
fn round_with_one_frame(
    executor: &mut QueueGroupExecutor,
    transmit: &mut SpscProducer<TxRequest>,
) -> GroupPollOutcome {
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    executor.poll(256)
}

#[test]
fn queue_poll_outcome_is_unchanged_by_the_observation_port() {
    let identity = port_identity(3);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    // The same round shape runs twice: the comparison has to cover a round
    // that moves a frame, because two empty rounds agree trivially.
    let _port = begin_port_capture(false);
    let closed = round_with_one_frame(&mut executor, &mut transmit);
    assert!(
        reports_of(identity).is_empty(),
        "a closed gate must not report"
    );
    assert!(
        matches!(closed, GroupPollOutcome::Idle(work) if work > 0),
        "the compared round must do real work"
    );

    publish_queue_poll_gate(true);
    let open = round_with_one_frame(&mut executor, &mut transmit);
    let reports = reports_of(identity);
    assert_eq!(reports.len(), 1);

    assert_eq!(closed, open, "observation must not change the poll outcome");
    assert_eq!(reports[0].work_units, open.work());
    close_port_gates();
}

#[test]
fn queue_rearm_reports_every_non_idle_outcome() {
    let _port = begin_port_capture(true);
    let identity = port_identity(5);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    // A rearm that ends idle is the plain case and produces no event.
    executor
        .shared
        .state
        .store(STATE_POLLING, Ordering::Release);
    executor.finish_idle();
    assert!(
        rearms_of(identity).is_empty(),
        "an idle rearm is not an event"
    );

    // An IRQ that arrived while the round was polling skips the hardware rearm.
    executor
        .shared
        .state
        .store(STATE_POLLING | STATE_MISSED, Ordering::Release);
    executor.finish_idle();
    assert_eq!(rearms_of(identity), vec![QueueRearmOutcome::Race]);

    // Rearm found work in the window.
    executor.group.irq_control = Box::new(ScriptedIrq {
        rearm: Some(NetRearmResult::WorkPending(rd_net::NetIrqSnapshot::RX)),
        fail: false,
    });
    executor
        .shared
        .state
        .store(STATE_POLLING, Ordering::Release);
    executor.finish_idle();
    assert_eq!(
        rearms_of(identity),
        vec![QueueRearmOutcome::Race, QueueRearmOutcome::WorkPending],
        "one rearm reports one outcome"
    );

    // The device asked for a deferred retry.
    executor.group.irq_control = Box::new(ScriptedIrq {
        rearm: Some(NetRearmResult::RetryAt { deadline_nanos: 1 }),
        fail: false,
    });
    executor
        .shared
        .state
        .store(STATE_POLLING, Ordering::Release);
    executor.finish_idle();
    assert_eq!(
        rearms_of(identity),
        vec![
            QueueRearmOutcome::Race,
            QueueRearmOutcome::WorkPending,
            QueueRearmOutcome::RetryAt,
        ],
        "one rearm reports one outcome"
    );
    assert!(executor.retry_at.is_some());

    // A failed rearm disables the group.
    executor.group.irq_control = Box::new(ScriptedIrq {
        rearm: None,
        fail: true,
    });
    executor
        .shared
        .state
        .store(STATE_POLLING, Ordering::Release);
    executor.finish_idle();
    assert_eq!(
        rearms_of(identity),
        vec![
            QueueRearmOutcome::Race,
            QueueRearmOutcome::WorkPending,
            QueueRearmOutcome::RetryAt,
            QueueRearmOutcome::Failed,
        ],
        "one rearm reports one outcome"
    );
    assert!(
        executor.shared.is_disabled(),
        "a failed rearm must leave the group disabled"
    );
    close_port_gates();
}

#[test]
fn tx_submit_reports_accepted_frames_and_backpressure_reports_refusals() {
    let _port = begin_port_capture(true);
    let identity = port_identity(6);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor(identity, Arc::clone(&trace), tx_ready);

    // An accepted frame is a submit event, not backpressure.
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    assert_eq!(executor.poll(256), GroupPollOutcome::Idle(1));
    let submits = submits_of(identity);
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].len, 60);
    assert!(backpressures_of(identity).is_empty());

    // A device that asks to wait: the frame is retained and the refusal is
    // reported instead of a submit.
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let device = rd_net::prepare_device(
        Box::new(ScriptedTxDevice {
            trace: Arc::clone(&trace),
            refusal: NetError::Retry,
        }),
        dma,
    )
    .unwrap();
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor_from_device(identity, tx_ready, device);
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let _ = executor.poll(256);
    let refusals = backpressures_of(identity);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].stage, QueueBackpressureStage::TxSubmit);
    assert_eq!(refusals[0].reason, QueueBackpressureReason::Retry);
    assert_eq!(
        submits_of(identity).len(),
        1,
        "a refused frame must not be reported as submitted"
    );
    assert!(
        executor.pending_tx.is_some(),
        "a retryable refusal keeps the frame retained"
    );

    // A link-down is retryable on the TX side too: the frame stays retained
    // and the report carries the classified reason.
    let device = rd_net::prepare_device(
        Box::new(ScriptedTxDevice {
            trace: Arc::clone(&trace),
            refusal: NetError::LinkDown,
        }),
        DeviceDma::new(
            DmaDeviceInfo::new(
                DmaDomainId::Direct,
                DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            &TEST_DMA,
        ),
    )
    .unwrap();
    let (mut transmit, tx_ready) = spsc_ring(2);
    let link_down_identity = port_identity(11);
    let mut executor = port_test_executor_from_device(link_down_identity, tx_ready, device);
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );
    let _ = executor.poll(256);
    let refusals = backpressures_of(link_down_identity);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].stage, QueueBackpressureStage::TxSubmit);
    assert_eq!(refusals[0].reason, QueueBackpressureReason::LinkDown);
    assert!(submits_of(link_down_identity).is_empty());
    assert!(
        executor.pending_tx.is_some(),
        "a link-down keeps the frame retained as well"
    );
    close_port_gates();
}

#[test]
fn a_permanent_refusal_is_reported_as_neither_a_submit_nor_backpressure() {
    let _port = begin_port_capture(true);
    let identity = port_identity(12);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let device = rd_net::prepare_device(
        Box::new(ScriptedTxDevice {
            trace: Arc::clone(&trace),
            refusal: NetError::NotSupported,
        }),
        dma,
    )
    .unwrap();
    let (mut transmit, tx_ready) = spsc_ring(2);
    let mut executor = port_test_executor_from_device(identity, tx_ready, device);
    let buffer = executor.group.tx_pool.allocate(60).unwrap();
    assert!(
        transmit
            .push(TxRequest {
                buffer,
                options: TxSubmitOptions {
                    notify: TxNotify::Deferred,
                    ..Default::default()
                },
            })
            .is_ok()
    );

    let _ = executor.poll(256);
    assert!(
        submits_of(identity).is_empty(),
        "a refused frame is not accepted"
    );
    assert!(
        backpressures_of(identity).is_empty(),
        "a permanent refusal is not backpressure"
    );
    assert!(
        executor.pending_tx.is_none(),
        "a permanent refusal drops the frame instead of retaining it"
    );
    close_port_gates();
}

#[test]
fn rx_refill_backpressure_reports_the_retry_and_keeps_the_replacement() {
    let _port = begin_port_capture(true);
    let identity = port_identity(13);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    let dma = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let device = rd_net::prepare_device(Box::new(RxRetryDevice(Arc::clone(&trace))), dma).unwrap();
    let mut executor = port_test_executor_from_device(identity, tx_ready, device);
    let replacement = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx_refill.push_back(PendingRxRefill {
        completion: None,
        replacement,
    });

    let _ = executor.poll(256);
    let refusals = backpressures_of(identity);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].stage, QueueBackpressureStage::RxRefill);
    assert_eq!(refusals[0].reason, QueueBackpressureReason::Retry);
    assert_eq!(
        executor.pending_rx_refill.len(),
        1,
        "the refused replacement stays retained"
    );
    assert!(
        !executor.shared.is_disabled(),
        "a retryable refill refusal does not fail the round"
    );
    close_port_gates();
}

#[test]
fn a_link_down_during_rx_refill_fails_the_round_instead_of_reporting_backpressure() {
    let _port = begin_port_capture(true);
    let identity = port_identity(15);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    // This RX queue refuses replacements permanently, so the refusal is a
    // link-down: `(RxRefill, LinkDown)` is not a reachable report.
    let mut executor =
        port_test_executor_with_rx_initial(identity, Arc::clone(&trace), tx_ready, 0);
    let replacement = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx_refill.push_back(PendingRxRefill {
        completion: None,
        replacement,
    });

    let outcome = executor.poll(256);
    assert!(matches!(outcome, GroupPollOutcome::Failed(_)));
    assert!(
        backpressures_of(identity).is_empty(),
        "a link-down during RX refill is a round failure, not backpressure"
    );
    assert!(executor.shared.is_disabled());
    close_port_gates();
}

#[test]
fn rx_publish_reports_frames_handed_to_the_protocol_side() {
    let _port = begin_port_capture(true);
    let identity = port_identity(7);
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (_tx, tx_ready) = spsc_ring(2);
    // Two accepted replacements: the reclaimed frame can be reposted, and
    // publishing happens once its replacement is accepted.
    let mut executor =
        port_test_executor_with_rx_initial(identity, Arc::clone(&trace), tx_ready, 2);
    let replacement = executor.group.rx.allocate_replacement().unwrap();
    executor.pending_rx_refill.push_back(PendingRxRefill {
        completion: None,
        replacement,
    });

    let _ = executor.poll(256);
    let published = publishes_of(identity);
    assert_eq!(published.len(), 1, "the reclaimed frame is published once");
    assert_eq!(published[0].len, 60);
    close_port_gates();
}
