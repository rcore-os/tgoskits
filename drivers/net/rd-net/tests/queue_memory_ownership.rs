use std::{
    alloc::{alloc_zeroed, dealloc},
    collections::VecDeque,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_kspin_test_runtime as _;
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rd_net::{
    ActiveQueueSet, DmaBuffer, DriverGeneric, IRxQueue, ITxQueue, Interface, Net, NetDeviceOwner,
    NetError, QueueConfig, QueueMemoryMode, RxQueueOwner, TxQueueOwner,
};

static LEGACY_SYNC_FOR_DEVICE: AtomicUsize = AtomicUsize::new(0);
static LEGACY_SYNC_FOR_CPU: AtomicUsize = AtomicUsize::new(0);
static AGGREGATE_SYNC_FOR_DEVICE: AtomicUsize = AtomicUsize::new(0);
static AGGREGATE_SYNC_FOR_CPU: AtomicUsize = AtomicUsize::new(0);
static LEGACY_DMA: TrackingDma = TrackingDma {
    sync_for_device: &LEGACY_SYNC_FOR_DEVICE,
    sync_for_cpu: &LEGACY_SYNC_FOR_CPU,
};
static AGGREGATE_DMA: TrackingDma = TrackingDma {
    sync_for_device: &AGGREGATE_SYNC_FOR_DEVICE,
    sync_for_cpu: &AGGREGATE_SYNC_FOR_CPU,
};

struct TrackingDma {
    sync_for_device: &'static AtomicUsize,
    sync_for_cpu: &'static AtomicUsize,
}

impl DmaOp for TrackingDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: `layout` comes from dma-api and the returned allocation is
        // retained until the matching `dealloc_contiguous` call below.
        let cpu_addr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe { DmaAllocHandle::new(cpu_addr, (cpu_addr.as_ptr() as u64).into(), layout) })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: the handle was produced by `alloc_contiguous` with this
        // exact pointer and layout, and dma-api drops it exactly once.
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        Err(DmaError::NoMemory)
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {
        unreachable!("this test never creates streaming mappings")
    }

    fn sync_alloc_for_device(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_for_device.fetch_add(1, Ordering::Relaxed);
    }

    fn sync_alloc_for_cpu(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_for_cpu.fetch_add(1, Ordering::Relaxed);
    }
}

struct TestInterface {
    mode: QueueMemoryMode,
}

impl DriverGeneric for TestInterface {
    fn name(&self) -> &str {
        "queue-memory-test"
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Interface for TestInterface {
    fn mac_address(&self) -> [u8; 6] {
        [0x02, 0, 0, 0, 0, 1]
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        Some(Box::new(TestTxQueue {
            mode: self.mode,
            completed: VecDeque::new(),
        }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        Some(Box::new(TestRxQueue {
            mode: self.mode,
            submitted: VecDeque::new(),
        }))
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }
}

struct TestTxQueue {
    mode: QueueMemoryMode,
    completed: VecDeque<u64>,
}

impl ITxQueue for TestTxQueue {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        queue_config(self.mode)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if self.mode == QueueMemoryMode::OwnerCopy {
            // SAFETY: OwnerCopy leaves CPU ownership with this queue method;
            // rd-net retains the allocation for the request lifetime.
            assert_eq!(unsafe { *buffer.virt.as_ptr() }, 0x5a);
        }
        self.completed.push_back(buffer.bus_addr);
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.completed.pop_front()
    }
}

struct TestRxQueue {
    mode: QueueMemoryMode,
    submitted: VecDeque<(u64, usize)>,
}

impl IRxQueue for TestRxQueue {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        queue_config(self.mode)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.submitted
            .push_back((buffer.bus_addr, buffer.virt.as_ptr() as usize));
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let (bus_addr, virt_addr) = self.submitted.pop_front()?;
        // This models an OwnerCopy driver copying a packet from its private
        // DMA arena into the runtime-owned CPU buffer before returning it.
        // SAFETY: rd-net retains the live buffer until this queue returns its
        // matching bus address from `reclaim`.
        unsafe { (virt_addr as *mut u8).write(0xa5) };
        Some((bus_addr, 1))
    }
}

const fn queue_config(memory_mode: QueueMemoryMode) -> QueueConfig {
    QueueConfig {
        dma_mask: u64::MAX,
        align: 8,
        buf_size: 64,
        ring_size: 2,
        memory_mode,
    }
}

fn exercise(memory_mode: QueueMemoryMode) -> (usize, usize, u8) {
    let mut active = Net::new(TestInterface { mode: memory_mode }, &LEGACY_DMA)
        .activate_queues()
        .unwrap_or_else(|_| panic!("valid test queues must activate"));

    // Ignore pool construction and RX prefill; only transfers after queue
    // activation are part of this ownership assertion.
    LEGACY_SYNC_FOR_DEVICE.store(0, Ordering::Relaxed);
    LEGACY_SYNC_FOR_CPU.store(0, Ordering::Relaxed);

    let (_, mut pending) = active
        .prepare_send(0, 1, |packet| packet[0] = 0x5a)
        .expect("TX buffer must be available");
    pending.try_submit().expect("TX submit must succeed");
    drop(pending);
    assert_eq!(active.reclaim_tx(0, 1).expect("TX reclaim must succeed"), 1);

    let received = active
        .receive(0, |packet| packet[0])
        .expect("RX reclaim must succeed")
        .expect("one RX packet must be ready");

    (
        LEGACY_SYNC_FOR_DEVICE.load(Ordering::Relaxed),
        LEGACY_SYNC_FOR_CPU.load(Ordering::Relaxed),
        received,
    )
}

fn exercise_aggregate_owner(memory_mode: QueueMemoryMode) -> (usize, usize, u8) {
    let mut active = Net::new_owned(AggregateOwner::new(memory_mode), &AGGREGATE_DMA)
        .activate_queues()
        .unwrap_or_else(|_| panic!("valid aggregate owner must activate"));

    AGGREGATE_SYNC_FOR_DEVICE.store(0, Ordering::Relaxed);
    AGGREGATE_SYNC_FOR_CPU.store(0, Ordering::Relaxed);

    let (_, mut pending) = active
        .prepare_send(0, 1, |packet| packet[0] = 0x5a)
        .expect("TX buffer must be available");
    pending.try_submit().expect("TX submit must succeed");
    drop(pending);
    assert_eq!(active.reclaim_tx(0, 1).unwrap(), 1);
    let received = active
        .receive(0, |packet| packet[0])
        .unwrap()
        .expect("one RX packet must be ready");

    (
        AGGREGATE_SYNC_FOR_DEVICE.load(Ordering::Relaxed),
        AGGREGATE_SYNC_FOR_CPU.load(Ordering::Relaxed),
        received,
    )
}

struct AggregateOwner {
    mode: QueueMemoryMode,
    tx: TestTxQueue,
    rx: TestRxQueue,
    activated: bool,
}

impl AggregateOwner {
    fn new(mode: QueueMemoryMode) -> Self {
        Self {
            mode,
            tx: TestTxQueue {
                mode,
                completed: VecDeque::new(),
            },
            rx: TestRxQueue {
                mode,
                submitted: VecDeque::new(),
            },
            activated: false,
        }
    }
}

impl DriverGeneric for AggregateOwner {
    fn name(&self) -> &str {
        "aggregate-owner-test"
    }
}

impl NetDeviceOwner for AggregateOwner {
    fn mac_address(&self) -> [u8; 6] {
        [0x02, 0, 0, 0, 0, 2]
    }

    fn activate_queue_set(&mut self) -> Result<ActiveQueueSet, NetError> {
        if self.activated {
            return Err(NetError::Retry);
        }
        self.activated = true;
        ActiveQueueSet::single(queue_config(self.mode), queue_config(self.mode))
            .map_err(|error| NetError::Other(Box::new(error)))
    }

    fn submit_tx(&mut self, queue: &TxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        assert_eq!(queue.id(), 0);
        self.tx.submit(buffer)
    }

    fn reclaim_tx(&mut self, queue: &TxQueueOwner) -> Result<Option<u64>, NetError> {
        assert_eq!(queue.id(), 0);
        Ok(self.tx.reclaim())
    }

    fn submit_rx(&mut self, queue: &RxQueueOwner, buffer: DmaBuffer) -> Result<(), NetError> {
        assert_eq!(queue.id(), 0);
        self.rx.submit(buffer)
    }

    fn reclaim_rx(&mut self, queue: &RxQueueOwner) -> Result<Option<(u64, usize)>, NetError> {
        assert_eq!(queue.id(), 0);
        Ok(self.rx.reclaim())
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }
}

#[test]
fn direct_dma_transfers_cache_ownership_but_owner_copy_does_not() {
    assert_eq!(exercise(QueueMemoryMode::DirectDma), (2, 1, 0xa5));
    assert_eq!(exercise(QueueMemoryMode::OwnerCopy), (0, 0, 0xa5));
}

#[test]
fn aggregate_owner_keeps_control_and_queue_state_in_one_mutable_object() {
    assert_eq!(
        exercise_aggregate_owner(QueueMemoryMode::OwnerCopy),
        (0, 0, 0xa5)
    );
}
