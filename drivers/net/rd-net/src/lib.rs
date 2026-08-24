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

    /// Transfers one prepared token to the device.
    pub fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError> {
        buffer.prepare_for_device();
        self.queue.submit(buffer)
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

    /// Posts fresh buffers up to `budget` or until the queue reaches capacity.
    pub fn initial_refill(&mut self, budget: usize) -> Result<usize, NetError> {
        let mut submitted = 0;
        while submitted < budget && self.posted < self.capacity() {
            let buffer = self.pool.allocate(self.config.buf_size)?;
            match self.submit(buffer) {
                Ok(()) => submitted += 1,
                Err(error) => {
                    let (_, reason) = error.into_parts();
                    if matches!(reason, NetError::Retry) {
                        break;
                    }
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
        completion.buffer.complete_for_cpu(completion.packet_len);
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
    validate_parts(&parts)?;

    let mut poll_groups = Vec::with_capacity(parts.poll_groups.len());
    for group in parts.poll_groups {
        let tx_config = group.queues.tx.config();
        let rx_config = group.queues.rx.config();
        let tx_pool = make_pool(&device_dma, tx_config, DmaDirection::ToDevice)?;
        let rx_pool = make_pool(&device_dma, rx_config, DmaDirection::FromDevice)?;
        poll_groups.push(PreparedNetPollGroup {
            id: group.id,
            tx: TxQueue::new(group.queues.tx),
            rx: RxQueue::new(group.queues.rx, rx_pool),
            tx_pool,
            irq_control: group.irq_control,
            irq_endpoints: group.irq_endpoints,
        });
    }

    Ok(PreparedNetDevice {
        info: parts.info,
        control: parts.control,
        wifi_control: parts.wifi_control,
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
    use core::{num::NonZeroUsize, ptr::NonNull};

    use dma_api::{DmaAllocHandle, DmaConstraints, DmaError, DmaMapHandle};

    use super::*;

    struct TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate contiguous DMA")
        }

        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {
            panic!("test should not deallocate contiguous DMA")
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            panic!("test should not allocate coherent DMA")
        }

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) -> Result<(), DmaError> {
            panic!("test should not deallocate coherent DMA")
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            panic!("test should not map streaming DMA")
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {
            panic!("test should not unmap streaming DMA")
        }
    }

    #[test]
    fn queue_dma_preserves_coherency_and_narrows_only_the_address_mask() {
        static DMA: TestDma = TestDma;
        let device = DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX)
                    .with_align(128)
                    .with_boundary(4096)
                    .with_max_segment_size(8192),
            ),
            &DMA,
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
