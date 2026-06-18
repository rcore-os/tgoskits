//! RDIF block-device bridge for SDIO-backed SD/MMC hosts.
//!
//! This module owns the reusable queue/runtime-independent part of adapting a
//! [`crate::sdio::SdioSdmmc`] card to [`rdif_block`]. Host crates provide the
//! small controller-specific [`BlockHost`] impl that submits and polls one
//! block request.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use log::warn;
use rdif_block::dma_api::DeviceDma;
pub use rdif_block::{BlkError, IQueue, Interface, Request, RequestId, RequestStatus, dma_api};

use crate::{
    BlockPoll, BlockRequestId, BlockTransferMode, Error,
    sdio::{SdioHost, SdioIrqHandle, SdioIrqHost, SdioSdmmc, block_queue_ready_from_host_event},
};

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_DMA_MASK: u64 = u32::MAX as u64;
pub const DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST: u32 = u16::MAX as u32 + 1;

#[derive(Clone)]
pub struct BlockConfig {
    pub name: &'static str,
    pub capacity_blocks: u64,
    pub dma_mask: u64,
    pub max_blocks_per_request: u32,
    pub max_segment_size: usize,
    pub irq_driven: bool,
    pub dma: Option<DeviceDma>,
}

impl BlockConfig {
    pub fn dma(name: &'static str, capacity_blocks: u64, irq_driven: bool, dma: DeviceDma) -> Self {
        let dma_mask = dma.dma_mask();
        Self {
            name,
            capacity_blocks,
            dma_mask,
            max_blocks_per_request: DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
            max_segment_size: usize::MAX,
            irq_driven,
            dma: Some(dma),
        }
    }

    pub const fn fifo(name: &'static str, capacity_blocks: u64, irq_driven: bool) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: DEFAULT_DMA_MASK,
            max_blocks_per_request: 1,
            max_segment_size: BLOCK_SIZE,
            irq_driven,
            dma: None,
        }
    }

    pub fn with_dma_mask(mut self, dma_mask: u64) -> Self {
        self.dma_mask = dma_mask;
        self
    }

    pub fn with_max_blocks_per_request(mut self, max_blocks_per_request: u32) -> Self {
        self.max_blocks_per_request = max_blocks_per_request;
        self
    }

    pub fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.max_segment_size = max_segment_size;
        self
    }

    pub fn with_irq_driven(mut self, irq_driven: bool) -> Self {
        self.irq_driven = irq_driven;
        self
    }

    pub fn with_dma(mut self, dma: DeviceDma) -> Self {
        self.dma_mask = dma.dma_mask();
        self.dma = Some(dma);
        self
    }

    pub const fn uses_dma(&self) -> bool {
        self.dma.is_some()
    }
}

pub trait BlockHost: SdioIrqHost + Send + Sync + 'static {
    type Request: Send + 'static;
    type Slot: Default + Send + 'static;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError>;

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError>;

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error>;

    fn request_id(request: &Self::Request) -> BlockRequestId;
}

pub struct BlockDevice<H>
where
    H: BlockHost,
{
    raw: Option<SharedCore<SdioSdmmc<H>>>,
    irq_handle: <H as SdioIrqHost>::IrqHandle,
    config: BlockConfig,
    irq_enabled: AtomicBool,
    queue_created: bool,
    irq_handler_taken: bool,
}

impl<H> BlockDevice<H>
where
    H: BlockHost,
{
    pub fn new(card: SdioSdmmc<H>, config: BlockConfig) -> Self {
        let raw = SharedCore::new(card);
        let irq_handle = raw.with_mut(|raw| raw.host().irq_handle());
        Self {
            raw: Some(raw),
            irq_handle,
            config,
            irq_enabled: AtomicBool::new(false),
            queue_created: false,
            irq_handler_taken: false,
        }
    }

    pub fn config(&self) -> &BlockConfig {
        &self.config
    }

    fn queue_limits_with_mask(&self, dma_mask: u64) -> rdif_block::QueueLimits {
        queue_limits(&self.config, dma_mask)
    }
}

impl<H> rdif_block::DriverGeneric for BlockDevice<H>
where
    H: BlockHost,
{
    fn name(&self) -> &str {
        self.config.name
    }
}

impl<H> Interface for BlockDevice<H>
where
    H: BlockHost,
{
    fn device_info(&self) -> rdif_block::DeviceInfo {
        device_info(&self.config)
    }

    fn queue_limits(&self) -> rdif_block::QueueLimits {
        self.queue_limits_with_mask(self.config.dma_mask)
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        if self.queue_created {
            return None;
        }
        self.raw.clone().map(|dev| {
            self.queue_created = true;
            Box::new(BlockQueue::<H>::new(dev, self.config.clone(), 0)) as _
        })
    }

    fn enable_irq(&self) {
        if !self.config.irq_driven {
            self.irq_enabled.store(false, Ordering::Release);
            return;
        }
        if let Some(raw) = &self.raw {
            let mut enabled = false;
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::enable_completion_irq(raw.host_mut()) {
                    warn!(
                        "{}: enable completion IRQ failed: {:?}",
                        self.config.name, err
                    );
                    return;
                }
                enabled = raw.host().completion_irq_enabled();
            });
            self.irq_enabled.store(enabled, Ordering::Release);
        }
    }

    fn disable_irq(&self) {
        if let Some(raw) = &self.raw {
            raw.with_mut(|raw| {
                if let Err(err) = SdioHost::disable_completion_irq(raw.host_mut()) {
                    warn!(
                        "{}: disable completion IRQ failed: {:?}",
                        self.config.name, err
                    );
                }
            });
        }
        self.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        if !self.config.irq_driven {
            return Vec::new();
        }
        vec![rdif_block::IrqSourceInfo::legacy(
            rdif_block::IdList::from_bits(1),
        )]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn rdif_block::IrqHandler>> {
        if !self.config.irq_driven || source_id != 0 || self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(BlockIrqHandler::<H> {
            handle: self.irq_handle.clone(),
            _marker: PhantomData,
        }))
    }
}

pub struct BlockQueue<H>
where
    H: BlockHost,
{
    raw: SharedCore<SdioSdmmc<H>>,
    config: BlockConfig,
    id: usize,
    slot: H::Slot,
    pending: Option<H::Request>,
    completed: Vec<RequestId>,
}

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    fn new(raw: SharedCore<SdioSdmmc<H>>, config: BlockConfig, id: usize) -> Self {
        Self {
            raw,
            config,
            id,
            slot: H::Slot::default(),
            pending: None,
            completed: Vec::new(),
        }
    }

    fn queue_info(&self) -> rdif_block::QueueInfo {
        rdif_block::IQueue::info(self)
    }

    fn submit_request_inner(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        rdif_block::validate_request(self.queue_info(), &request)?;
        self.reap_pending_request()?;
        let raw = self.raw.clone();
        raw.with_mut(|raw| {
            let start_block = block_addr_for_card(request.lba, raw.is_high_capacity())?;
            let buffer = request
                .segments
                .first()
                .copied()
                .ok_or(BlkError::InvalidRequest)?;
            if !buffer.len().is_multiple_of(BLOCK_SIZE) {
                return Err(BlkError::Other("buffer is not block aligned"));
            }
            let ptr = NonNull::new(buffer.virt).ok_or(BlkError::Other("buffer pointer is null"))?;
            let size = NonZeroUsize::new(buffer.len()).ok_or(BlkError::Other("buffer is empty"))?;
            let dma = self.config.dma.as_ref();
            let id = match request.op {
                rdif_block::RequestOp::Read => H::submit_read_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    dma,
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Write => H::submit_write_request(
                    raw.host_mut(),
                    start_block,
                    ptr,
                    size,
                    dma,
                    &mut self.slot,
                    &mut self.pending,
                )?,
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => return Err(BlkError::NotSupported),
            };
            Ok(RequestId::new(usize::from(id)))
        })
    }

    fn poll_request_inner(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        if let Some(index) = self.completed.iter().position(|id| *id == request) {
            self.completed.swap_remove(index);
            return Ok(RequestStatus::Complete);
        }
        self.poll_active_request(request)
    }

    fn poll_active_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let raw = self.raw.clone();
        match raw.with_mut(|raw| {
            H::poll_block_request(
                raw.host_mut(),
                &mut self.pending,
                BlockRequestId::new(usize::from(request)),
                &mut self.slot,
            )
        }) {
            Ok(BlockPoll::Complete) => Ok(RequestStatus::Complete),
            Ok(BlockPoll::Pending) => Ok(RequestStatus::Pending),
            Err(err) => Err(map_dev_err_to_blk_err(err)),
        }
    }

    fn pending_id(&self) -> Option<BlockRequestId> {
        self.pending.as_ref().map(H::request_id)
    }

    fn reap_pending_request(&mut self) -> Result<RequestStatus, BlkError> {
        let Some(active) = self.pending_id() else {
            return Ok(RequestStatus::Complete);
        };
        let id = RequestId::new(usize::from(active));
        match self.poll_active_request(id) {
            Ok(RequestStatus::Complete) => {
                self.completed.push(id);
                Ok(RequestStatus::Complete)
            }
            Ok(RequestStatus::Pending) => Err(BlkError::Retry),
            Err(err) => Err(err),
        }
    }
}

// SAFETY: `BlockQueue` owns one pending request slot. The concrete host
// request object owns any borrowed request segment until task-side poll
// reports completion or error.
unsafe impl<H> IQueue for BlockQueue<H>
where
    H: BlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: device_info(&self.config),
            limits: queue_limits(&self.config, self.config.dma_mask),
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        self.submit_request_inner(request)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        self.poll_request_inner(request)
    }
}

struct BlockIrqHandler<H>
where
    H: BlockHost,
{
    handle: <H as SdioIrqHost>::IrqHandle,
    _marker: PhantomData<H>,
}

impl<H> rdif_block::IrqHandler for BlockIrqHandler<H>
where
    H: BlockHost,
{
    fn handle_irq(&self) -> rdif_block::Event {
        let host_event = self.handle.handle_irq();
        let mut event = rdif_block::Event::none();
        if let Some(queue_id) = block_queue_ready_from_host_event(&host_event) {
            event.push_queue(queue_id);
        }
        event
    }
}

pub fn queue_limits(config: &BlockConfig, dma_mask: u64) -> rdif_block::QueueLimits {
    rdif_block::QueueLimits {
        dma_mask,
        dma_alignment: BLOCK_SIZE,
        max_inflight: 1,
        max_blocks_per_request: config.max_blocks_per_request,
        max_segments: 1,
        max_segment_size: config.max_segment_size,
        supported_flags: rdif_block::RequestFlags::NONE,
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}

pub fn device_info(config: &BlockConfig) -> rdif_block::DeviceInfo {
    rdif_block::DeviceInfo {
        name: Some(config.name),
        ..rdif_block::DeviceInfo::new(config.capacity_blocks, BLOCK_SIZE)
    }
}

pub fn block_addr_for_card(block_id: u64, high_capacity: bool) -> Result<u32, BlkError> {
    let block_id = u32::try_from(block_id).map_err(|_| BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(BlkError::InvalidBlockIndex(block_id as u64))
    }
}

pub fn map_dev_err_to_blk_err(err: Error) -> BlkError {
    match err {
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => BlkError::Io,
    }
}

pub fn transfer_mode_for_dma(dma: Option<&DeviceDma>) -> BlockTransferMode {
    match dma {
        Some(_) => BlockTransferMode::Dma,
        None => BlockTransferMode::Fifo,
    }
}

pub fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

struct SharedCore<T> {
    inner: Arc<SharedCoreInner<T>>,
}

struct SharedCoreInner<T> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
}

struct SharedCoreGuard<'a, T> {
    inner: &'a SharedCoreInner<T>,
}

// SAFETY: `SharedCore` serializes all mutable access through a single atomic
// borrow flag. IRQ top halves use host-specific cloneable handles instead.
unsafe impl<T: Send> Send for SharedCoreInner<T> {}

// SAFETY: See the `Send` impl.
unsafe impl<T: Send> Sync for SharedCoreInner<T> {}

impl<T> SharedCore<T> {
    fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SharedCoreInner {
                value: UnsafeCell::new(value),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.enter();
        f(guard.get_mut())
    }
}

impl<T> Clone for SharedCore<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> SharedCoreInner<T> {
    fn enter(&self) -> SharedCoreGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_enter() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }

    fn try_enter(&self) -> Option<SharedCoreGuard<'_, T>> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(SharedCoreGuard { inner: self })
    }
}

impl<T> SharedCoreGuard<'_, T> {
    fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<T> Drop for SharedCoreGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::{
        CommandResponsePoll, DataCommandPoll,
        cmd::Command,
        sdio::{ClockSpeed, HostEvent, HostEventKind},
    };

    #[test]
    fn fifo_config_limits_single_block_requests() {
        let config = BlockConfig::fifo("test-sdmmc", 8, true);
        let limits = queue_limits(&config, DEFAULT_DMA_MASK);

        assert_eq!(limits.max_inflight, 1);
        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, BLOCK_SIZE);
        assert!(!limits.supports_flush);
    }

    #[test]
    fn disabled_irq_policy_does_not_advertise_sources() {
        let device = BlockDevice::new(
            SdioSdmmc::new(MockHost::default()),
            BlockConfig::fifo("mock-sd", 8, false),
        );

        assert!(Interface::irq_sources(&device).is_empty());
    }

    #[test]
    fn enabled_irq_handler_maps_host_event_to_queue_zero() {
        let mut device = BlockDevice::new(
            SdioSdmmc::new(MockHost::default()),
            BlockConfig::fifo("mock-sd", 8, true),
        );
        let handler = Interface::take_irq_handler(&mut device, 0).unwrap();

        let event = handler.handle_irq();

        assert!(event.queues.contains(0));
        assert!(!event.is_empty());
    }

    #[test]
    fn poll_request_only_completes_matching_request_id() {
        let mut queue = BlockQueue::<MockHost>::new(
            SharedCore::new(SdioSdmmc::new(MockHost::default())),
            BlockConfig::fifo("mock-sd", 8, false),
            0,
        );
        queue.pending = Some(MockRequest {
            id: BlockRequestId::new(7),
        });

        assert_eq!(
            queue.poll_request_inner(RequestId::new(8)),
            Ok(RequestStatus::Pending)
        );
        assert_eq!(
            queue.poll_request_inner(RequestId::new(7)),
            Ok(RequestStatus::Complete)
        );
        assert!(queue.pending.is_none());
    }

    #[test]
    fn unsupported_ops_are_rejected() {
        let mut queue = BlockQueue::<MockHost>::new(
            SharedCore::new(SdioSdmmc::new(MockHost::default())),
            BlockConfig::fifo("mock-sd", 8, false),
            0,
        );
        let mut segments = [];
        let request = Request {
            op: rdif_block::RequestOp::Flush,
            lba: 0,
            block_count: 0,
            segments: &mut segments,
            flags: rdif_block::RequestFlags::NONE,
        };

        assert_eq!(queue.submit_request(request), Err(BlkError::NotSupported));
    }

    #[derive(Clone, Default)]
    struct MockIrqHandle;

    impl SdioIrqHandle for MockIrqHandle {
        type Event = MockEvent;

        fn handle_irq(&self) -> Self::Event {
            MockEvent(HostEventKind::TransferComplete)
        }
    }

    #[derive(Clone, Copy, Default)]
    struct MockEvent(HostEventKind);

    impl HostEvent for MockEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }
    }

    #[derive(Default)]
    struct MockHost {
        irq_enabled: AtomicBool,
        next_id: AtomicUsize,
    }

    #[derive(Default)]
    struct MockSlot;

    struct MockRequest {
        id: BlockRequestId,
    }

    impl SdioHost for MockHost {
        type Event = MockEvent;
        type DataRequest<'a> = ();

        fn submit_command(&mut self, _cmd: &Command) -> Result<(), Error> {
            Err(Error::UnsupportedCommand)
        }

        fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
            Ok(CommandResponsePoll::Pending)
        }

        fn submit_read_data<'a>(
            &mut self,
            _cmd: &Command,
            _buf: &'a mut [u8],
            _block_size: u32,
            _block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            Err(Error::UnsupportedCommand)
        }

        fn submit_write_data<'a>(
            &mut self,
            _cmd: &Command,
            _buf: &'a [u8],
            _block_size: u32,
            _block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            Err(Error::UnsupportedCommand)
        }

        fn poll_data_request<'a>(
            &mut self,
            _request: &mut Self::DataRequest<'a>,
        ) -> Result<DataCommandPoll, Error> {
            Err(Error::UnsupportedCommand)
        }

        fn set_bus_width(&mut self, _width: crate::sdio::BusWidth) -> Result<(), Error> {
            Ok(())
        }

        fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
            Ok(())
        }

        fn enable_completion_irq(&mut self) -> Result<(), Error> {
            self.irq_enabled.store(true, Ordering::Release);
            Ok(())
        }

        fn disable_completion_irq(&mut self) -> Result<(), Error> {
            self.irq_enabled.store(false, Ordering::Release);
            Ok(())
        }
    }

    impl SdioIrqHost for MockHost {
        type IrqHandle = MockIrqHandle;

        fn irq_handle(&self) -> Self::IrqHandle {
            MockIrqHandle
        }

        fn completion_irq_enabled(&self) -> bool {
            self.irq_enabled.load(Ordering::Acquire)
        }
    }

    impl BlockHost for MockHost {
        type Request = MockRequest;
        type Slot = MockSlot;

        fn submit_read_request(
            &mut self,
            _start_block: u32,
            _buffer: NonNull<u8>,
            _size: NonZeroUsize,
            _dma: Option<&DeviceDma>,
            _slot: &mut Self::Slot,
            pending: &mut Option<Self::Request>,
        ) -> Result<BlockRequestId, BlkError> {
            self.submit_mock_request(pending)
        }

        fn submit_write_request(
            &mut self,
            _start_block: u32,
            _buffer: NonNull<u8>,
            _size: NonZeroUsize,
            _dma: Option<&DeviceDma>,
            _slot: &mut Self::Slot,
            pending: &mut Option<Self::Request>,
        ) -> Result<BlockRequestId, BlkError> {
            self.submit_mock_request(pending)
        }

        fn poll_block_request(
            &mut self,
            pending: &mut Option<Self::Request>,
            request: BlockRequestId,
            _slot: &mut Self::Slot,
        ) -> Result<BlockPoll, Error> {
            match pending.as_ref() {
                Some(active) if active.id == request => {
                    *pending = None;
                    Ok(BlockPoll::Complete)
                }
                Some(_) => Ok(BlockPoll::Pending),
                None => Ok(BlockPoll::Complete),
            }
        }

        fn request_id(request: &Self::Request) -> BlockRequestId {
            request.id
        }
    }

    impl MockHost {
        fn submit_mock_request(
            &self,
            pending: &mut Option<MockRequest>,
        ) -> Result<BlockRequestId, BlkError> {
            if pending.is_some() {
                return Err(BlkError::Retry);
            }
            let id = BlockRequestId::new(self.next_id.fetch_add(1, Ordering::Relaxed));
            *pending = Some(MockRequest { id });
            Ok(id)
        }
    }
}
