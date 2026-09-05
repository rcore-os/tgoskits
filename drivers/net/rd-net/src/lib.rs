#![no_std]

extern crate alloc;

use alloc::{boxed::Box, collections::BTreeSet, vec::Vec};
use core::alloc::Layout;

use dma_api::{ContiguousBufferPool, DeviceDma, DmaDirection};
pub use rdif_eth::*;

fn other_error(message: &'static str) -> NetError {
    NetError::Other(Box::new(KError::Unknown(message)))
}

/// DMA allocation pool shared with one queue ownership pipeline.
#[derive(Clone)]
pub struct NetBufferPool {
    pool: ContiguousBufferPool,
    buffer_size: usize,
}

impl NetBufferPool {
    fn new(pool: ContiguousBufferPool, buffer_size: usize) -> Self {
        Self { pool, buffer_size }
    }

    /// Allocates a move-only token with the requested logical length.
    pub fn allocate(&self, len: usize) -> Result<DmaBuffer, NetError> {
        if len > self.buffer_size {
            return Err(NetError::InvalidParts);
        }
        let buffer = self.pool.alloc()?;
        DmaBuffer::new(buffer, len).map_err(|_| NetError::InvalidParts)
    }

    /// Returns the allocation size of every token in this pool.
    pub const fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

/// Runtime-owned TX hardware queue.
pub struct TxQueue {
    queue: Box<dyn ITxQueue>,
    config: QueueConfig,
}

impl TxQueue {
    fn new(queue: Box<dyn ITxQueue>) -> Self {
        let config = queue.config();
        Self { queue, config }
    }

    /// Returns the device-local queue identifier.
    pub fn id(&self) -> NetQueueId {
        self.queue.id()
    }

    /// Returns the hardware queue capacity.
    pub fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    /// Returns transport checksums this queue can calculate.
    pub fn checksum_capabilities(&self) -> TxChecksumCapabilities {
        self.queue.checksum_capabilities()
    }

    /// Transfers one prepared token to the device.
    pub fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        buffer.prepare_for_device();
        self.queue.submit(buffer)
    }

    /// Transfers one prepared token with checksum and notification options.
    pub fn submit_with_options(
        &mut self,
        buffer: DmaBuffer,
        options: TxSubmitOptions,
    ) -> Result<(), SubmitError> {
        buffer.prepare_for_device();
        self.queue.submit_with_options(buffer, options)
    }

    /// Makes all deferred submissions visible to the device.
    pub fn flush(&mut self) {
        self.queue.flush();
    }

    /// Reclaims one completed token from the device.
    pub fn reclaim(&mut self) -> Option<DmaBuffer> {
        self.queue.reclaim()
    }
}

/// Runtime-owned RX hardware queue and its refill pool.
pub struct RxQueue {
    queue: Box<dyn IRxQueue>,
    pool: NetBufferPool,
    config: QueueConfig,
    posted: usize,
}

impl RxQueue {
    fn new(queue: Box<dyn IRxQueue>, pool: NetBufferPool) -> Self {
        let config = queue.config();
        Self {
            queue,
            pool,
            config,
            posted: 0,
        }
    }

    /// Returns the device-local queue identifier.
    pub fn id(&self) -> NetQueueId {
        self.queue.id()
    }

    /// Returns the number of buffers the queue can own concurrently.
    pub fn capacity(&self) -> usize {
        self.config.ring_size.saturating_sub(1)
    }

    /// Allocates one queue-compatible token for replacement-before-delivery.
    pub fn allocate_replacement(&self) -> Result<DmaBuffer, NetError> {
        self.pool.allocate(self.config.buf_size)
    }

    /// Posts fresh buffers up to `budget` or until the queue reaches capacity.
    pub fn initial_refill(&mut self, budget: usize) -> Result<usize, NetError> {
        let mut submitted = 0;
        while submitted < budget && self.posted < self.capacity() {
            let buffer = self.pool.allocate(self.config.buf_size)?;
            match self.submit(buffer) {
                Ok(()) => submitted += 1,
                Err(error) => {
                    let (_, reason) = error.into_parts();
                    return Err(reason);
                }
            }
        }
        Ok(submitted)
    }

    /// Returns one completed packet and synchronizes its payload for the CPU.
    pub fn reclaim(&mut self) -> Option<RxCompletion> {
        let completion = self.queue.reclaim()?;
        self.posted = self
            .posted
            .checked_sub(1)
            .expect("RX driver reclaimed more buffers than were posted");
        // A corrupted descriptor may report a length beyond both the queue
        // contract and its actual allocation. Synchronize only memory the
        // token owns, while preserving the raw length for the protocol owner
        // to reject and recycle deterministically.
        let sync_len = completion
            .packet_len
            .min(self.config.buf_size)
            .min(completion.buffer.capacity());
        completion.buffer.complete_for_cpu(sync_len);
        Some(completion)
    }

    /// Recycles a protocol-consumed token back to the device.
    pub fn recycle(&mut self, mut buffer: DmaBuffer) -> Result<(), SubmitError> {
        if let Err(error) = buffer.set_len(self.config.buf_size) {
            return Err(SubmitError::new(buffer, error));
        }
        self.submit(buffer)
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        buffer.prepare_for_device();
        self.queue.submit(buffer)?;
        self.posted += 1;
        Ok(())
    }

    /// Returns the number of tokens currently owned by hardware.
    pub const fn posted(&self) -> usize {
        self.posted
    }
}

/// Prepared task-context ownership for one driver poll group.
pub struct PreparedNetPollGroup {
    /// Stable driver group identifier.
    pub id: NetPollGroupId,
    /// Exclusive TX queue.
    pub tx: TxQueue,
    /// Exclusive RX queue.
    pub rx: RxQueue,
    /// TX allocation pool used by the protocol owner.
    pub tx_pool: NetBufferPool,
    /// Task-context mask/rearm endpoint.
    pub irq_control: Box<dyn NetPollIrqControl>,
    /// Optional one-shot initialization executed by the fixed owner CPU.
    pub owner_startup: Option<Box<dyn NetOwnerStartup>>,
    /// Move-only hard IRQ endpoints.
    pub irq_endpoints: Vec<NetHardIrqEndpoint>,
}

/// A portable device after its complete object has been consumed and its DMA
/// pools have been allocated, but before workers or IRQs are started.
pub struct PreparedNetDevice {
    /// Static device information.
    pub info: NetDeviceInfo,
    /// Exclusive general control endpoint.
    pub control: Box<dyn NetControlEndpoint>,
    /// Optional exclusive wireless control endpoint.
    pub wifi_control: Option<Box<dyn WifiControl>>,
    /// Every prepared poll group.
    pub poll_groups: Vec<PreparedNetPollGroup>,
}

/// Consumes a portable device into independently owned queue/control parts and
/// allocates every queue's DMA pool. No device IRQ is armed by this function.
pub fn prepare_device(
    device: Box<dyn NetDevice>,
    device_dma: DeviceDma,
) -> Result<PreparedNetDevice, NetError> {
    let parts = device.into_parts()?;
    if let Err(error) = validate_parts(&parts) {
        // `into_parts` may already have exposed descriptor memory to the
        // device. No owner CPU exists yet, so retain the complete device
        // backing instead of running lifecycle control from an arbitrary CPU.
        core::mem::forget(parts);
        return Err(error);
    }

    let mut pools = Vec::with_capacity(parts.poll_groups.len());
    for group in &parts.poll_groups {
        let tx_config = group.queues.tx.config();
        let rx_config = group.queues.rx.config();
        let tx_pool = match make_pool(&device_dma, tx_config, DmaDirection::ToDevice) {
            Ok(pool) => pool,
            Err(error) => {
                core::mem::forget(parts);
                return Err(error);
            }
        };
        let rx_pool = match make_pool(&device_dma, rx_config, DmaDirection::FromDevice) {
            Ok(pool) => pool,
            Err(error) => {
                core::mem::forget(parts);
                return Err(error);
            }
        };
        pools.push((tx_pool, rx_pool));
    }

    let NetDeviceParts {
        info,
        control,
        wifi_control,
        poll_groups: raw_poll_groups,
    } = parts;

    let mut poll_groups = Vec::with_capacity(raw_poll_groups.len());
    for (group, (tx_pool, rx_pool)) in raw_poll_groups.into_iter().zip(pools) {
        poll_groups.push(PreparedNetPollGroup {
            id: group.id,
            tx: TxQueue::new(group.queues.tx),
            rx: RxQueue::new(group.queues.rx, rx_pool),
            tx_pool,
            irq_control: group.irq_control,
            owner_startup: group.owner_startup,
            irq_endpoints: group.irq_endpoints,
        });
    }

    Ok(PreparedNetDevice {
        info,
        control,
        wifi_control,
        poll_groups,
    })
}

fn validate_parts(parts: &NetDeviceParts) -> Result<(), NetError> {
    if parts.poll_groups.is_empty() {
        return Err(NetError::InvalidParts);
    }

    let mut group_ids = BTreeSet::new();
    let mut queue_ids = BTreeSet::new();
    for group in &parts.poll_groups {
        if !group_ids.insert(group.id) || group.irq_endpoints.is_empty() {
            return Err(NetError::InvalidParts);
        }
        if !queue_ids.insert((true, group.queues.tx.id()))
            || !queue_ids.insert((false, group.queues.rx.id()))
        {
            return Err(NetError::InvalidParts);
        }
        if group.queues.tx.config().ring_size < 2 || group.queues.rx.config().ring_size < 2 {
            return Err(NetError::InvalidParts);
        }
    }
    Ok(())
}

fn make_pool(
    device_dma: &DeviceDma,
    config: QueueConfig,
    direction: DmaDirection,
) -> Result<NetBufferPool, NetError> {
    let layout = Layout::from_size_align(config.buf_size, config.align.max(1))
        .map_err(|_| other_error("invalid queue DMA layout"))?;
    let dma = queue_dma(device_dma, config);
    let pool = dma.contiguous_buffer_pool(layout, direction, config.ring_size);
    Ok(NetBufferPool::new(pool, config.buf_size))
}

fn queue_dma(device_dma: &DeviceDma, config: QueueConfig) -> DeviceDma {
    let mut constraints = device_dma.info().constraints();
    constraints.addr_mask = constraints.addr_mask.min(config.dma_mask);
    device_dma.with_constraints(constraints)
}

#[cfg(test)]
mod tests {
    use alloc::{
        alloc::{alloc_zeroed, dealloc},
        collections::VecDeque,
    };
    use core::{num::NonZeroUsize, ptr::NonNull};

    use dma_api::{DmaAllocHandle, DmaConstraints, DmaError, DmaMapHandle};

    use super::*;

    struct TestDma;

    impl TestDma {
        unsafe fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(unsafe {
                DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
            })
        }
    }

    impl dma_api::DmaOp for TestDma {
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
            Ok(unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    struct RetryRxQueue;

    struct PlainTxQueue;

    impl ITxQueue for PlainTxQueue {
        fn id(&self) -> NetQueueId {
            NetQueueId::new(0)
        }

        fn config(&self) -> QueueConfig {
            QueueConfig {
                dma_mask: u64::MAX,
                align: 64,
                buf_size: 256,
                ring_size: 2,
            }
        }

        fn submit(&mut self, _buffer: DmaBuffer) -> Result<(), SubmitError> {
            panic!("a checksum request must not fall through to plain submit")
        }

        fn reclaim(&mut self) -> Option<DmaBuffer> {
            None
        }
    }

    impl IRxQueue for RetryRxQueue {
        fn id(&self) -> NetQueueId {
            NetQueueId::new(0)
        }

        fn config(&self) -> QueueConfig {
            QueueConfig {
                dma_mask: u64::MAX,
                align: 64,
                buf_size: 256,
                ring_size: 2,
            }
        }

        fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
            Err(SubmitError::new(buffer, NetError::Retry))
        }

        fn reclaim(&mut self) -> Option<RxCompletion> {
            None
        }
    }

    struct ScriptedRxQueue {
        buffers: VecDeque<DmaBuffer>,
        packet_lengths: VecDeque<usize>,
    }

    impl IRxQueue for ScriptedRxQueue {
        fn id(&self) -> NetQueueId {
            NetQueueId::new(0)
        }

        fn config(&self) -> QueueConfig {
            QueueConfig {
                dma_mask: u64::MAX,
                align: 64,
                buf_size: 256,
                ring_size: 3,
            }
        }

        fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
            self.buffers.push_back(buffer);
            Ok(())
        }

        fn reclaim(&mut self) -> Option<RxCompletion> {
            Some(RxCompletion {
                buffer: self.buffers.pop_front()?,
                packet_len: self.packet_lengths.pop_front()?,
            })
        }
    }

    fn test_device_dma() -> DeviceDma {
        static DMA: TestDma = TestDma;
        DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            &DMA,
        )
    }

    #[test]
    fn unsupported_checksum_submission_returns_the_move_only_token() {
        let config = PlainTxQueue.config();
        let pool = make_pool(&test_device_dma(), config, DmaDirection::ToDevice).unwrap();
        let buffer = pool.allocate(128).unwrap();
        let bus_addr = buffer.bus_addr();
        let mut tx = TxQueue::new(Box::new(PlainTxQueue));

        let error = tx
            .submit_with_options(
                buffer,
                TxSubmitOptions::immediate(Some(TxChecksumOffload {
                    network: TxNetworkProtocol::Ipv4,
                    transport: TxTransportProtocol::Tcp,
                    transport_offset: 34,
                })),
            )
            .unwrap_err();
        let (buffer, reason) = error.into_parts();

        assert!(matches!(reason, NetError::NotSupported));
        assert_eq!(buffer.bus_addr(), bus_addr);
        assert_eq!(buffer.len(), 128);
    }

    #[test]
    fn initial_refill_propagates_retry_instead_of_publishing_an_empty_queue() {
        let config = RetryRxQueue.config();
        let pool = make_pool(&test_device_dma(), config, DmaDirection::FromDevice).unwrap();
        let mut rx = RxQueue::new(Box::new(RetryRxQueue), pool);

        let error = rx.initial_refill(1).unwrap_err();

        assert!(matches!(error, NetError::Retry));
        assert_eq!(rx.posted(), 0);
    }

    #[test]
    fn oversized_completion_is_safely_reclaimed_and_preserves_the_raw_length() {
        let queue = ScriptedRxQueue {
            buffers: VecDeque::new(),
            packet_lengths: VecDeque::from([257, 64]),
        };
        let config = queue.config();
        let pool = make_pool(&test_device_dma(), config, DmaDirection::FromDevice).unwrap();
        let mut rx = RxQueue::new(Box::new(queue), pool);
        assert_eq!(rx.initial_refill(2).unwrap(), 2);

        let oversized = rx.reclaim().expect("the first descriptor must complete");
        assert_eq!(oversized.packet_len, 257);
        assert_eq!(oversized.buffer.capacity(), 256);
        rx.recycle(oversized.buffer).unwrap();

        let valid = rx
            .reclaim()
            .expect("the next descriptor must remain visible");
        assert_eq!(valid.packet_len, 64);
        assert_eq!(rx.posted(), 1);
    }

    #[test]
    fn queue_dma_preserves_coherency_and_narrows_only_the_address_mask() {
        let device = test_device_dma().with_constraints(
            DmaConstraints::new(u64::MAX)
                .with_align(128)
                .with_boundary(4096)
                .with_max_segment_size(8192),
        );
        let queue = queue_dma(
            &device,
            QueueConfig {
                dma_mask: u32::MAX as u64,
                align: 64,
                buf_size: 2048,
                ring_size: 32,
            },
        );

        assert_eq!(queue.info().coherency(), dma_api::DmaCoherency::Coherent);
        assert_eq!(queue.info().constraints().addr_mask, u32::MAX as u64);
        assert_eq!(queue.info().constraints().align, 128);
        assert_eq!(queue.info().constraints().boundary, Some(4096));
        assert_eq!(queue.info().constraints().max_segment_size, Some(8192));
    }
}
