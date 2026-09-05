use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull, sync::atomic::AtomicUsize};
use std::{
    alloc::{alloc_zeroed, dealloc},
    sync::Mutex as StdMutex,
};

use irq_framework::{HwIrq, IrqDomainId};
use rd_net::{
    DmaBuffer, NetControlEndpoint, NetDeviceInfo, PreparedNetDevice, RxCompletion,
    TxNetworkProtocol, TxNotify, TxSubmitOptions, TxTransportProtocol, WifiOperation,
    WifiTransaction, Wpa2Pmk,
    dma_api::{
        DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
        DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    },
};

use super::*;
use crate::device::{EthernetFramePort, NetDeviceError, ProtocolEthernetFrame};

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct DroppingControl(Arc<AtomicUsize>);

impl Drop for DroppingControl {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl NetControlEndpoint for DroppingControl {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        Ok([0; 6])
    }
}

struct UnexpectedRegistrar;

impl PinnedNetIrqRegistrar for UnexpectedRegistrar {
    fn register(
        &self,
        _name: String,
        _irq: IrqId,
        _owner_cpu: usize,
        _action: PinnedNetIrqAction,
    ) -> Result<Box<dyn PinnedNetIrqRegistration>, PinnedNetIrqError> {
        panic!("zero-CPU topology must fail before IRQ registration")
    }
}

pub(super) struct TestDma;

impl TestDma {
    unsafe fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe {
            DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
        })
    }
}

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::allocate(layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(
            unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            },
        )
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

pub(super) static TEST_DMA: TestDma = TestDma;

fn dma_buffer(capacity: usize, len: usize) -> DmaBuffer {
    let device = DeviceDma::new(
        DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::Coherent,
            DmaConstraints::new(u64::MAX),
        ),
        &TEST_DMA,
    );
    let pool = device.contiguous_buffer_pool(
        Layout::from_size_align(capacity, 64).unwrap(),
        DmaDirection::Bidirectional,
        1,
    );
    DmaBuffer::new(pool.alloc().unwrap(), len)
        .unwrap_or_else(|_| panic!("test DMA token length exceeds its allocation"))
}

fn tx_frame(marker: u8) -> ProtocolEthernetFrame {
    let mut frame = ProtocolEthernetFrame::new(60).unwrap();
    frame.packet_mut().fill(marker);
    frame
}

fn tx_frame_marker(buffer: &DmaBuffer) -> u8 {
    buffer.read_with_cpu(buffer.len(), |packet| packet[0])
}

fn tx_test_port(
    tx_queue_discipline: TxQueueDiscipline,
    initial_tx_tokens: usize,
) -> (
    QueueFramePort,
    SpscConsumer<executor::TxRequest>,
    SpscProducer<DmaBuffer>,
) {
    let (_rx_ready_tx, rx_ready) = spsc_ring(1);
    let (rx_recycle, _rx_recycle_rx) = spsc_ring(1);
    let (tx_ready, tx_ready_rx) = spsc_ring(4);
    let (mut tx_free_tx, tx_free) = spsc_ring(4);
    for _ in 0..initial_tx_tokens {
        tx_free_tx.push(dma_buffer(2048, 0)).unwrap();
    }
    let shared = Arc::new(group_state(STATE_IDLE));
    let group = ProtocolGroupPort {
        rx_ready,
        rx_recycler: Arc::new(executor::RxRecycler::new(
            rx_recycle,
            Arc::clone(&shared),
            1,
        )),
        tx_ready,
        tx_free,
        tx_spares: Vec::new(),
        shared,
    };
    (
        QueueFramePort {
            name: String::from("test0"),
            mac: Arc::new(SpinLock::new([0; 6])),
            groups: vec![group],
            tx_queue_discipline,
            pending_tx: VecDeque::new(),
            next_rx: 0,
            next_tx: 0,
            checksum_capabilities: rd_net::TxChecksumCapabilities::NONE,
        },
        tx_ready_rx,
        tx_free_tx,
    )
}

#[test]
fn fifo_backlog_is_lazy_bounded_ordered_and_flushes_after_token_return() {
    let (mut port, mut tx_ready, mut tx_free) = tx_test_port(
        TxQueueDiscipline::Fifo {
            max_frames: NonZeroUsize::new(2).unwrap(),
        },
        1,
    );

    assert_eq!(port.pending_tx.capacity(), 0);
    assert_eq!(port.transmit(&tx_frame(1)), Ok(()));
    assert_eq!(port.transmit(&tx_frame(2)), Ok(()));
    assert_eq!(port.transmit(&tx_frame(3)), Ok(()));
    assert_eq!(port.pending_tx.len(), 2);
    assert_eq!(port.transmit(&tx_frame(4)), Err(NetDeviceError::Again));

    let first = tx_ready.pop().unwrap();
    assert_eq!(tx_frame_marker(&first.buffer), 1);
    tx_free.push(first.buffer).unwrap();
    assert!(matches!(port.receive(), Err(NetDeviceError::Again)));
    assert_eq!(port.pending_tx.len(), 1);

    let second = tx_ready.pop().unwrap();
    assert_eq!(tx_frame_marker(&second.buffer), 2);
    tx_free.push(second.buffer).unwrap();
    assert!(matches!(port.receive(), Err(NetDeviceError::Again)));
    assert!(port.pending_tx.is_empty());

    let third = tx_ready.pop().unwrap();
    assert_eq!(tx_frame_marker(&third.buffer), 3);
}

#[test]
fn fifo_direct_fill_and_backlog_preserve_submit_options() {
    let (mut port, mut tx_ready, mut tx_free) = tx_test_port(
        TxQueueDiscipline::Fifo {
            max_frames: NonZeroUsize::new(1).unwrap(),
        },
        1,
    );
    let options = TxSubmitOptions {
        notify: TxNotify::Deferred,
        ..Default::default()
    };
    let mut filled_address = 0;
    port.transmit_frame_with_options(100, options, &mut |packet| {
        filled_address = packet.as_ptr() as usize;
        packet.fill(0x31);
    })
    .unwrap();
    let first = tx_ready.pop().unwrap();
    assert_eq!(first.options, options);
    first.buffer.read_with_cpu(100, |packet| {
        assert_eq!(packet.as_ptr() as usize, filled_address);
        assert_eq!(packet, &[0x31; 100]);
    });
    assert_eq!(port.pending_tx.capacity(), 0);

    let mut fills = 0;
    port.transmit_frame_with_options(100, options, &mut |packet| {
        fills += 1;
        packet.fill(0x32);
    })
    .unwrap();
    assert_eq!(fills, 1);
    assert_eq!(port.pending_tx.len(), 1);
    tx_free.push(first.buffer).unwrap();
    // The production adapter uses receive_owned, which must also flush FIFO.
    assert!(matches!(port.receive_owned(), Err(NetDeviceError::Again)));
    let second = tx_ready.pop().unwrap();
    assert_eq!(second.options, options);
    second
        .buffer
        .read_with_cpu(100, |packet| assert_eq!(packet, &[0x32; 100]));
    assert!(port.pending_tx.is_empty());
}

#[test]
fn noqueue_never_retains_or_allocates_when_device_is_busy() {
    let (mut port, _tx_ready, _tx_free) = tx_test_port(TxQueueDiscipline::NoQueue, 0);

    assert_eq!(port.pending_tx.capacity(), 0);
    assert_eq!(port.transmit(&tx_frame(1)), Err(NetDeviceError::Again));
    assert!(port.pending_tx.is_empty());
    assert_eq!(port.pending_tx.capacity(), 0);
}

#[test]
fn device_qdisc_limits_and_backlogs_are_isolated() {
    let (mut first, _first_ready, _first_free) = tx_test_port(
        TxQueueDiscipline::Fifo {
            max_frames: NonZeroUsize::new(1).unwrap(),
        },
        0,
    );
    let (mut second, _second_ready, _second_free) = tx_test_port(
        TxQueueDiscipline::Fifo {
            max_frames: NonZeroUsize::new(2).unwrap(),
        },
        0,
    );

    assert_eq!(first.transmit(&tx_frame(1)), Ok(()));
    assert_eq!(first.transmit(&tx_frame(2)), Err(NetDeviceError::Again));
    assert_eq!(second.transmit(&tx_frame(3)), Ok(()));
    assert_eq!(second.transmit(&tx_frame(4)), Ok(()));
    assert_eq!(first.pending_tx.len(), 1);
    assert_eq!(second.pending_tx.len(), 2);
}

struct RecordingRegistration {
    id: usize,
    order: Arc<StdMutex<Vec<usize>>>,
}

impl PinnedNetIrqRegistration for RecordingRegistration {
    fn owner_cpu(&self) -> usize {
        0
    }

    fn enable(&self) -> Result<(), PinnedNetIrqError> {
        Ok(())
    }

    fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError> {
        self.order.lock().unwrap().push(self.id);
        Ok(())
    }
}

struct FailingRegistration {
    drops: Arc<AtomicUsize>,
}

impl Drop for FailingRegistration {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl PinnedNetIrqRegistration for FailingRegistration {
    fn owner_cpu(&self) -> usize {
        0
    }

    fn enable(&self) -> Result<(), PinnedNetIrqError> {
        Ok(())
    }

    fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError> {
        Err(PinnedNetIrqError::Other)
    }
}

#[derive(Clone, Copy)]
enum ModelOperation {
    Publish,
    Rearm,
    FinishMore,
    Disable,
}

fn irq(line: u32) -> IrqId {
    IrqId::new(IrqDomainId(1), HwIrq(line))
}

#[test]
fn spsc_ring_is_bounded_and_preserves_move_order() {
    let (mut producer, mut consumer) = spsc_ring(2);
    assert!(producer.push(10).is_ok());
    assert!(producer.push(20).is_ok());
    assert_eq!(producer.push(30), Err(30));
    assert_eq!(consumer.pop(), Some(10));
    assert_eq!(consumer.pop(), Some(20));
    assert_eq!(consumer.pop(), None);
}

#[test]
fn failed_initialization_unwinds_irq_leases_in_reverse_order() {
    let order = Arc::new(StdMutex::new(Vec::new()));
    let registrations = (0..3)
        .map(|id| {
            Box::new(RecordingRegistration {
                id,
                order: Arc::clone(&order),
            }) as Box<dyn PinnedNetIrqRegistration>
        })
        .collect::<Vec<_>>();

    assert!(disable_registrations(&registrations));
    assert_eq!(*order.lock().unwrap(), vec![2, 1, 0]);
}

#[test]
fn unsynchronized_irq_registration_is_quarantined() {
    let drops = Arc::new(AtomicUsize::new(0));
    let registrations = vec![Box::new(FailingRegistration {
        drops: Arc::clone(&drops),
    }) as Box<dyn PinnedNetIrqRegistration>];

    assert!(!release_registrations(registrations));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
}

#[test]
fn unsynchronized_irq_quarantines_runtime_side_control_ownership() {
    let drops = Arc::new(AtomicUsize::new(0));

    release_runtime_side_resources(DropProbe(Arc::clone(&drops)), false);

    assert_eq!(drops.load(Ordering::Relaxed), 0);
    release_runtime_side_resources(DropProbe(Arc::clone(&drops)), true);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn spsc_ring_drops_each_move_only_token_exactly_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let (mut producer, consumer) = spsc_ring(1);
    assert!(producer.push(DropProbe(Arc::clone(&drops))).is_ok());
    let rejected = match producer.push(DropProbe(Arc::clone(&drops))) {
        Err(token) => token,
        Ok(()) => panic!("full ring must return ownership to the producer"),
    };
    drop(rejected);
    assert_eq!(drops.load(Ordering::Relaxed), 1);

    drop(producer);
    drop(consumer);
    assert_eq!(drops.load(Ordering::Relaxed), 2);
}

#[test]
fn unconfirmed_dma_shutdown_quarantines_hardware_visible_backing() {
    let drops = Arc::new(AtomicUsize::new(0));

    release_or_quarantine(DropProbe(Arc::clone(&drops)), false);

    assert_eq!(drops.load(Ordering::Relaxed), 0);
    release_or_quarantine(DropProbe(Arc::clone(&drops)), true);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn driver_backing_requires_both_irq_and_dma_shutdown_proofs() {
    assert!(backing_can_be_released(true, true));
    assert!(!backing_can_be_released(false, true));
    assert!(!backing_can_be_released(true, false));
    assert!(!backing_can_be_released(false, false));
}

#[test]
fn startup_failure_waits_for_an_explicit_cleanup_command() {
    assert_eq!(requested_irq_synchronization(COMMAND_WAIT), None);
    assert_eq!(requested_irq_synchronization(COMMAND_START), None);
    assert_eq!(requested_irq_synchronization(COMMAND_STOP), Some(true));
    assert_eq!(
        requested_irq_synchronization(COMMAND_QUARANTINE),
        Some(false)
    );
}

#[test]
fn zero_cpu_topology_quarantines_prepared_device_ownership() {
    let drops = Arc::new(AtomicUsize::new(0));
    let input = NetworkDeviceInput {
        name: String::from("quarantine-test"),
        device: PreparedNetDevice {
            info: NetDeviceInfo::new("quarantine-test", [0; 6]),
            control: Box::new(DroppingControl(Arc::clone(&drops))),
            wifi_control: None,
            poll_groups: Vec::new(),
        },
        irq_sources: Vec::new(),
        tx_queue_discipline: TxQueueDiscipline::NoQueue,
    };

    let result = NetworkRuntimeBuilder::new(vec![input], &UnexpectedRegistrar, 0).build();

    assert!(matches!(result, Err(NetworkRuntimeError::InvalidTopology)));
    assert_eq!(drops.load(Ordering::Relaxed), 0);
}

#[test]
fn oversized_rx_frame_recycles_token_and_next_frame_remains_receivable() {
    let (mut rx_ready_tx, rx_ready_rx) = spsc_ring(2);
    let (mut rx_recycle_tx, mut rx_recycle_rx) = spsc_ring(1);
    let (tx_ready_tx, _tx_ready_rx) = spsc_ring(1);
    let (_tx_free_tx, tx_free_rx) = spsc_ring(1);
    let oversized = dma_buffer(4096, 4096);
    let oversized_bus_addr = oversized.bus_addr();
    let valid = dma_buffer(4096, 64);
    let valid_bus_addr = valid.bus_addr();
    let occupied = dma_buffer(4096, 64);
    let occupied_bus_addr = occupied.bus_addr();

    rx_ready_tx
        .push(RxCompletion {
            buffer: oversized,
            packet_len: 2049,
        })
        .unwrap();
    rx_ready_tx
        .push(RxCompletion {
            buffer: valid,
            packet_len: 64,
        })
        .unwrap();
    rx_recycle_tx.push(occupied).unwrap();

    let shared = Arc::new(group_state(STATE_IDLE));
    let rx_recycler = Arc::new(RxRecycler::new(rx_recycle_tx, Arc::clone(&shared), 2));
    let mut port = ProtocolGroupPort {
        rx_ready: rx_ready_rx,
        rx_recycler,
        tx_ready: tx_ready_tx,
        tx_free: tx_free_rx,
        tx_spares: Vec::new(),
        shared,
    };

    assert!(matches!(port.receive(), Err(NetDeviceError::InvalidParam)));
    assert_eq!(port.rx_recycler.overflow_len(), 1);
    assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), occupied_bus_addr);

    let frame = port
        .receive()
        .expect("a malformed frame must not consume the next completion");
    assert_eq!(frame.packet_len(), 64);
    assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), valid_bus_addr);

    port.rx_recycler.flush_overflow();
    assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), oversized_bus_addr);
    assert_eq!(port.rx_recycler.overflow_len(), 0);

    let invalid_length = dma_buffer(2048, 2048);
    let invalid_length_bus_addr = invalid_length.bus_addr();
    rx_ready_tx
        .push(RxCompletion {
            buffer: invalid_length,
            packet_len: 2049,
        })
        .unwrap();
    assert!(matches!(port.receive(), Err(NetDeviceError::Io)));
    assert_eq!(
        rx_recycle_rx.pop().unwrap().bus_addr(),
        invalid_length_bus_addr
    );
}

#[test]
fn direct_rx_consumes_dma_backing_before_recycling_the_token() {
    let (mut rx_ready_tx, rx_ready_rx) = spsc_ring(1);
    let (rx_recycle_tx, mut rx_recycle_rx) = spsc_ring(1);
    let (tx_ready_tx, _tx_ready_rx) = spsc_ring(1);
    let (_tx_free_tx, tx_free_rx) = spsc_ring(1);
    let mut buffer = dma_buffer(4096, 4096);
    buffer.write_with_cpu(|packet| packet[..3000].fill(0x5a));
    let bus_addr = buffer.bus_addr();
    rx_ready_tx
        .push(RxCompletion {
            buffer,
            packet_len: 3000,
        })
        .unwrap();

    let shared = Arc::new(group_state(STATE_IDLE));
    let mut port = ProtocolGroupPort {
        rx_ready: rx_ready_rx,
        rx_recycler: Arc::new(RxRecycler::new(rx_recycle_tx, Arc::clone(&shared), 1)),
        tx_ready: tx_ready_tx,
        tx_free: tx_free_rx,
        tx_spares: Vec::new(),
        shared,
    };
    let consumed = port
        .receive_with(&mut |packet| {
            assert_eq!(packet.len(), 3000);
            assert!(packet.iter().all(|byte| *byte == 0x5a));
            packet.len()
        })
        .unwrap();

    assert_eq!(consumed, 3000);
    assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), bus_addr);
}

#[test]
fn detached_rx_retains_dma_until_the_owned_frame_is_dropped() {
    let (mut rx_ready_tx, rx_ready_rx) = spsc_ring(1);
    let (rx_recycle_tx, mut rx_recycle_rx) = spsc_ring(1);
    let (tx_ready_tx, _tx_ready_rx) = spsc_ring(1);
    let (_tx_free_tx, tx_free_rx) = spsc_ring(1);
    let mut buffer = dma_buffer(4096, 4096);
    buffer.write_with_cpu(|packet| packet[..3000].fill(0x6b));
    let bus_addr = buffer.bus_addr();
    rx_ready_tx
        .push(RxCompletion {
            buffer,
            packet_len: 3000,
        })
        .unwrap();

    let shared = Arc::new(group_state(STATE_IDLE));
    let mut port = ProtocolGroupPort {
        rx_ready: rx_ready_rx,
        rx_recycler: Arc::new(RxRecycler::new(rx_recycle_tx, Arc::clone(&shared), 1)),
        tx_ready: tx_ready_tx,
        tx_free: tx_free_rx,
        tx_spares: Vec::new(),
        shared,
    };
    let frame = port
        .receive_owned()
        .expect("the queued DMA frame must be returned as owned storage");

    assert!(rx_recycle_rx.pop().is_none());
    frame.read_with(|packet| {
        assert_eq!(packet.len(), 3000);
        assert!(packet.iter().all(|byte| *byte == 0x6b));
    });
    drop(frame);
    assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), bus_addr);
}

#[test]
fn direct_tx_fills_dma_and_preserves_submission_options() {
    let (_rx_ready_tx, rx_ready_rx) = spsc_ring(1);
    let (rx_recycle_tx, _rx_recycle_rx) = spsc_ring(1);
    let (tx_ready_tx, mut tx_ready_rx) = spsc_ring(1);
    let (mut tx_free_tx, tx_free_rx) = spsc_ring(1);
    tx_free_tx.push(dma_buffer(2048, 2048)).unwrap();
    let shared = Arc::new(group_state(STATE_IDLE));
    let mut port = ProtocolGroupPort {
        rx_ready: rx_ready_rx,
        rx_recycler: Arc::new(RxRecycler::new(rx_recycle_tx, Arc::clone(&shared), 1)),
        tx_ready: tx_ready_tx,
        tx_free: tx_free_rx,
        tx_spares: Vec::new(),
        shared,
    };
    let options = TxSubmitOptions::deferred(Some(rd_net::TxChecksumOffload {
        network: TxNetworkProtocol::Ipv4,
        transport: TxTransportProtocol::Tcp,
        transport_offset: 34,
    }));

    port.transmit_frame_with_options(100, options, &mut |packet| {
        packet.fill(0xa5);
    })
    .unwrap();

    let request = tx_ready_rx
        .pop()
        .expect("one DMA request must be published");
    assert_eq!(request.options, options);
    assert_eq!(request.buffer.len(), 100);
    request.buffer.read_with_cpu(100, |packet| {
        assert!(packet.iter().all(|byte| *byte == 0xa5));
    });
    assert_eq!(options.notify, TxNotify::Deferred);
}

fn group_state(initial: u8) -> PollGroupState {
    let state = PollGroupState::new(0, Arc::new(ax_task::IrqNotify::new()));
    state.state.store(initial, Ordering::Release);
    state
}

fn apply_model_operation(state: &PollGroupState, operation: ModelOperation) {
    match operation {
        ModelOperation::Publish => state.schedule_task(),
        ModelOperation::Rearm => {
            let _ = state.begin_rearm();
        }
        ModelOperation::FinishMore => state.finish_more(),
        ModelOperation::Disable => state.disable(),
    }
}

#[test]
fn shared_irq_groups_are_assigned_to_the_same_cpu() {
    let owners = assign_affinity_domains(&[vec![irq(4)], vec![irq(4)], vec![irq(5)]], 4);
    assert_eq!(owners[0], owners[1]);
    assert_ne!(owners[0], owners[2]);
}

#[test]
fn affinity_domains_merge_transitively_through_shared_sources() {
    let owners = assign_affinity_domains(
        &[
            vec![irq(1)],
            vec![irq(1), irq(2)],
            vec![irq(2)],
            vec![irq(3)],
        ],
        4,
    );
    assert_eq!(owners[0], owners[1]);
    assert_eq!(owners[1], owners[2]);
    assert_ne!(owners[2], owners[3]);
}

#[test]
fn independent_sources_can_use_different_cpus() {
    let owners = assign_affinity_domains(&[vec![irq(1)], vec![irq(2)]], 4);
    assert_eq!(owners, vec![0, 1]);
}

#[test]
fn missed_event_survives_poll_completion() {
    let notify = Arc::new(ax_task::IrqNotify::new());
    let state = PollGroupState::new(0, notify);
    state.activate(false);
    state.schedule_task();
    state.state.store(STATE_POLLING, Ordering::Release);
    state.schedule_task();
    assert!(!state.begin_rearm());
    assert_eq!(
        state.state.load(Ordering::Acquire) & STATE_MASK,
        STATE_SCHEDULED
    );
}

#[test]
fn rearm_window_is_linearizable_in_both_event_orders() {
    for operations in [
        [ModelOperation::Publish, ModelOperation::Rearm],
        [ModelOperation::Rearm, ModelOperation::Publish],
    ] {
        let state = group_state(STATE_POLLING);
        for operation in operations {
            apply_model_operation(&state, operation);
        }
        assert_eq!(
            state.state.load(Ordering::Acquire) & STATE_MASK,
            STATE_SCHEDULED
        );
    }
}

#[test]
fn disabled_group_cannot_be_resurrected_by_any_completion_order() {
    let permutations = [
        [
            ModelOperation::Publish,
            ModelOperation::FinishMore,
            ModelOperation::Disable,
        ],
        [
            ModelOperation::Publish,
            ModelOperation::Disable,
            ModelOperation::FinishMore,
        ],
        [
            ModelOperation::FinishMore,
            ModelOperation::Publish,
            ModelOperation::Disable,
        ],
        [
            ModelOperation::FinishMore,
            ModelOperation::Disable,
            ModelOperation::Publish,
        ],
        [
            ModelOperation::Disable,
            ModelOperation::Publish,
            ModelOperation::FinishMore,
        ],
        [
            ModelOperation::Disable,
            ModelOperation::FinishMore,
            ModelOperation::Publish,
        ],
    ];

    for operations in permutations {
        let state = group_state(STATE_POLLING);
        for operation in operations {
            apply_model_operation(&state, operation);
        }
        assert_eq!(
            state.state.load(Ordering::Acquire) & STATE_MASK,
            STATE_DISABLED
        );
    }
}

#[test]
fn queue_budget_only_reports_a_nonzero_exact_exhaustion() {
    assert!(!budget_was_exhausted(0, 0));
    assert!(!budget_was_exhausted(63, 64));
    assert!(budget_was_exhausted(64, 64));
}

#[test]
fn hardware_retry_rearms_instead_of_immediately_rescheduling() {
    assert!(matches!(
        hardware_retry_outcome(0),
        GroupPollOutcome::Idle(0)
    ));
    assert!(waits_for_hardware_event(&NetError::Retry));
    assert!(waits_for_hardware_event(&NetError::LinkDown));
    assert!(!waits_for_hardware_event(&NetError::NotSupported));
    assert!(matches!(
        rx_refill_retry_outcome(0, 0),
        GroupPollOutcome::Idle(0)
    ));
    assert!(matches!(
        rx_refill_retry_outcome(1, 1),
        GroupPollOutcome::More(1)
    ));
}

#[test]
fn protocol_owner_uses_the_least_loaded_cpu() {
    assert_eq!(select_protocol_owner(&[0, 0, 1], 4), 2);
    assert_eq!(select_protocol_owner(&[0, 1, 2, 3], 4), 0);
}

#[test]
fn secure_startup_transaction_receives_runtime_owned_entropy() {
    let transaction = WifiTransaction::connect_wpa2_pmk("ssid", Wpa2Pmk::new([0x11; 32]));
    let transaction =
        prepare_startup_transaction(transaction, || Ok::<_, crate::NetError>([0x22; 32])).unwrap();

    let WifiOperation::Connect { entropy, .. } = transaction.operation() else {
        panic!("expected station transaction");
    };
    assert_eq!(entropy, &Some([0x22; 32]));
}

#[test]
fn open_startup_transaction_does_not_consume_secure_entropy() {
    let transaction = prepare_startup_transaction(
        WifiTransaction::connect_open("ssid"),
        || -> Result<[u8; 32], crate::NetError> {
            panic!("open startup connection must not request secure entropy")
        },
    )
    .unwrap();

    assert!(!transaction.needs_connect_entropy());
}
