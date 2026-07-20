use std::{
    alloc::{alloc_zeroed, dealloc},
    collections::VecDeque,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use ax_kspin_test_runtime as _;
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rd_net::{
    DmaBuffer, DriverGeneric, IRxQueue, ITxQueue, Interface, Net, NetError, QueueConfig,
    QueueMemoryMode,
};

static ACTIVATION_DMA: TrackingDma = TrackingDma::new();
static ACTIVE_OWNER_DMA: TrackingDma = TrackingDma::new();
static RECYCLE_DMA: TrackingDma = TrackingDma::new();
static RECYCLE_FAULT_DMA: TrackingDma = TrackingDma::new();

struct TrackingDma {
    allocations: AtomicUsize,
    deallocations: AtomicUsize,
    sync_for_device: AtomicUsize,
    sync_for_cpu: AtomicUsize,
}

impl TrackingDma {
    const fn new() -> Self {
        Self {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
            sync_for_device: AtomicUsize::new(0),
            sync_for_cpu: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        self.allocations.store(0, Ordering::Release);
        self.deallocations.store(0, Ordering::Release);
        self.sync_for_device.store(0, Ordering::Release);
        self.sync_for_cpu.store(0, Ordering::Release);
    }
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
        // SAFETY: `layout` is retained in the returned handle and is used by
        // the matching deallocation below.
        let cpu_addr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        self.allocations.fetch_add(1, Ordering::AcqRel);
        Some(unsafe { DmaAllocHandle::new(cpu_addr, (cpu_addr.as_ptr() as u64).into(), layout) })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        self.deallocations.fetch_add(1, Ordering::AcqRel);
        // SAFETY: `handle` came from `alloc_contiguous` on this allocator and
        // dma-api invokes this operation at most once for the handle.
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
        unreachable!("these tests never create streaming mappings")
    }

    fn sync_alloc_for_device(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_for_device.fetch_add(1, Ordering::AcqRel);
    }

    fn sync_alloc_for_cpu(
        &self,
        _handle: &DmaAllocHandle,
        _offset: usize,
        _size: usize,
        _direction: DmaDirection,
    ) {
        self.sync_for_cpu.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn failed_partial_rx_activation_drop_retains_hardware_visible_dma() {
    ACTIVATION_DMA.reset();
    let accepted = Arc::new(AtomicUsize::new(0));
    let activation = Net::new(
        PartialActivationInterface {
            accepted: Arc::clone(&accepted),
        },
        &ACTIVATION_DMA,
    )
    .activate_queues();
    let failure = match activation {
        Ok(_) => panic!("the second RX admission must fail activation"),
        Err(failure) => failure,
    };

    assert_eq!(accepted.load(Ordering::Acquire), 1);
    assert!(ACTIVATION_DMA.allocations.load(Ordering::Acquire) >= 2);
    assert_eq!(ACTIVATION_DMA.deallocations.load(Ordering::Acquire), 0);

    drop(failure);
    assert_eq!(
        ACTIVATION_DMA.deallocations.load(Ordering::Acquire),
        0,
        "Drop must quarantine partially published DMA until quiescence is proven"
    );
}

#[test]
fn active_owner_drop_enters_quarantine_without_releasing_rx_dma() {
    ACTIVE_OWNER_DMA.reset();
    let state = Arc::new(Mutex::new(RecycleState::default()));
    let activation = Net::new(
        RecycleInterface {
            state: Arc::clone(&state),
            retry_once: true,
        },
        &ACTIVE_OWNER_DMA,
    )
    .activate_queues();
    let active = match activation {
        Ok(active) => active,
        Err(_) => panic!("the initial RX admission must succeed"),
    };

    assert!(ACTIVE_OWNER_DMA.allocations.load(Ordering::Acquire) > 0);
    drop(active);
    assert_eq!(
        ACTIVE_OWNER_DMA.deallocations.load(Ordering::Acquire),
        0,
        "a live owner cannot infer DMA quiescence from Rust Drop"
    );
}

#[test]
fn one_rx_return_retry_is_re_staged_without_shrinking_the_ring() {
    RECYCLE_DMA.reset();
    let state = Arc::new(Mutex::new(RecycleState::default()));
    let activation = Net::new(
        RecycleInterface {
            state: Arc::clone(&state),
            retry_once: true,
        },
        &RECYCLE_DMA,
    )
    .activate_queues();
    let mut active = match activation {
        Ok(active) => active,
        Err(_) => panic!("the initial RX admission must succeed"),
    };
    RECYCLE_DMA.deallocations.store(0, Ordering::Release);
    let device_sync_before = RECYCLE_DMA.sync_for_device.load(Ordering::Acquire);
    let cpu_sync_before = RECYCLE_DMA.sync_for_cpu.load(Ordering::Acquire);

    let packet = active
        .receive(0, |bytes| bytes[0])
        .expect("the first completed packet must be valid")
        .expect("the fixture publishes one RX completion");
    assert_eq!(packet, 0x7a);
    assert_eq!(RECYCLE_DMA.deallocations.load(Ordering::Acquire), 0);

    assert!(
        active
            .try_receive(0)
            .expect("the staged buffer must be retried by the queue owner")
            .is_none()
    );
    let state = state.lock().expect("recycle state lock");
    assert_eq!(state.submit_calls, 3);
    assert_eq!(state.accepted.len(), 1);
    assert_eq!(RECYCLE_DMA.deallocations.load(Ordering::Acquire), 0);
    assert_eq!(
        RECYCLE_DMA.sync_for_device.load(Ordering::Acquire) - device_sync_before,
        2,
        "the rejected buffer must be prepared again before the accepted retry"
    );
    assert_eq!(
        RECYCLE_DMA.sync_for_cpu.load(Ordering::Acquire) - cpu_sync_before,
        2,
        "RX reclaim and rejected admission must each restore CPU ownership"
    );
}

#[test]
fn terminal_rx_return_failure_is_reported_without_releasing_the_buffer() {
    RECYCLE_FAULT_DMA.reset();
    let state = Arc::new(Mutex::new(RecycleState::default()));
    let activation = Net::new(
        RecycleInterface {
            state,
            retry_once: false,
        },
        &RECYCLE_FAULT_DMA,
    )
    .activate_queues();
    let mut active = match activation {
        Ok(active) => active,
        Err(_) => panic!("the initial RX admission must succeed"),
    };
    RECYCLE_FAULT_DMA.deallocations.store(0, Ordering::Release);

    assert!(
        active
            .receive(0, |bytes| bytes[0])
            .expect("the completed packet remains consumable")
            .is_some()
    );
    assert!(matches!(active.try_receive(0), Err(NetError::LinkDown)));
    assert_eq!(
        RECYCLE_FAULT_DMA.deallocations.load(Ordering::Acquire),
        0,
        "a terminal return error must retain ownership for recovery"
    );
}

struct PartialActivationInterface {
    accepted: Arc<AtomicUsize>,
}

impl DriverGeneric for PartialActivationInterface {
    fn name(&self) -> &str {
        "partial-rx-activation"
    }
}

impl Interface for PartialActivationInterface {
    fn mac_address(&self) -> [u8; 6] {
        [0; 6]
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        Some(Box::new(IdleTxQueue))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        Some(Box::new(PartialActivationRxQueue {
            accepted: Arc::clone(&self.accepted),
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

struct PartialActivationRxQueue {
    accepted: Arc<AtomicUsize>,
}

impl IRxQueue for PartialActivationRxQueue {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        queue_config(4)
    }

    fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), NetError> {
        if self.accepted.load(Ordering::Acquire) == 0 {
            self.accepted.store(1, Ordering::Release);
            Ok(())
        } else {
            Err(NetError::LinkDown)
        }
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        None
    }
}

struct RecycleInterface {
    state: Arc<Mutex<RecycleState>>,
    retry_once: bool,
}

impl DriverGeneric for RecycleInterface {
    fn name(&self) -> &str {
        "rx-recycle"
    }
}

impl Interface for RecycleInterface {
    fn mac_address(&self) -> [u8; 6] {
        [0; 6]
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        Some(Box::new(IdleTxQueue))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        Some(Box::new(RecycleRxQueue {
            state: Arc::clone(&self.state),
            retry_once: self.retry_once,
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

#[derive(Default)]
struct RecycleState {
    submit_calls: usize,
    accepted: VecDeque<(u64, usize)>,
    completion_published: bool,
}

struct RecycleRxQueue {
    state: Arc<Mutex<RecycleState>>,
    retry_once: bool,
}

impl IRxQueue for RecycleRxQueue {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        queue_config(2)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        let mut state = self.state.lock().expect("recycle state lock");
        state.submit_calls += 1;
        if state.submit_calls == 2 {
            return Err(if self.retry_once {
                NetError::Retry
            } else {
                NetError::LinkDown
            });
        }
        state
            .accepted
            .push_back((buffer.bus_addr, buffer.virt.as_ptr() as usize));
        Ok(())
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        let mut state = self.state.lock().expect("recycle state lock");
        if state.completion_published {
            return None;
        }
        state.completion_published = true;
        let (bus_addr, virt) = state.accepted.pop_front()?;
        // SAFETY: rd-net retains this submitted allocation until the matching
        // bus address is reclaimed here.
        unsafe { (virt as *mut u8).write(0x7a) };
        Some((bus_addr, 1))
    }
}

struct IdleTxQueue;

impl ITxQueue for IdleTxQueue {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        queue_config(2)
    }

    fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), NetError> {
        Ok(())
    }

    fn reclaim(&mut self) -> Option<u64> {
        None
    }
}

const fn queue_config(ring_size: usize) -> QueueConfig {
    QueueConfig {
        dma_mask: u64::MAX,
        align: 8,
        buf_size: 64,
        ring_size,
        memory_mode: QueueMemoryMode::DirectDma,
    }
}
