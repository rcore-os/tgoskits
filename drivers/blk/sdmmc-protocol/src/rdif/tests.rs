use alloc::{
    alloc::{alloc_zeroed, dealloc},
    sync::Arc,
    vec::Vec,
};
use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rdif_block::dma_api::{CompletedDma, DeviceDma, PreparedDma};
use sdio_host2::{RequestPoll, SdioHost as PhysicalSdioHost, Transaction};

use super::*;
use crate::{
    BlockPoll, BlockRequestId, CommandResponsePoll, DataCommandPoll, Error, OperationPoll,
    cmd::Command,
    sdio::{
        card::SdioSdmmc,
        host::{ClockSpeed, HostEvent, HostEventKind, SdioHost, SdioIrqHandle, SdioIrqHost},
        host2::{SdioHost2Adapter, SdioHost2Irq},
    },
};

fn block_control(config: BlockConfig) -> Arc<BlockControl<MockHost>> {
    let raw = SharedCore::new(SdioSdmmc::new(MockHost::default()));
    Arc::new(BlockControl {
        raw: raw.clone(),
        config,
        irq_enabled: AtomicBool::new(false),
        queue_taken: AtomicBool::new(false),
    })
}

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
    let mut handler = Interface::take_irq_handler(&mut device, 0).unwrap();

    let event = handler.handle_irq();

    assert!(event.queues.contains(0));
    assert!(!event.is_empty());
}

#[test]
fn irq_handler_is_one_shot_and_source_checked() {
    let mut device = BlockDevice::new(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::fifo("mock-sd", 8, true),
    );

    assert!(Interface::take_irq_handler(&mut device, 1).is_none());
    assert!(Interface::take_irq_handler(&mut device, 0).is_some());
    assert!(Interface::take_irq_handler(&mut device, 0).is_none());
}

#[test]
fn irq_enable_disable_respects_policy() {
    let disabled = BlockDevice::new(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::fifo("mock-sd", 8, false),
    );

    assert!(!Interface::is_irq_enabled(&disabled));
    assert!(Interface::irq_sources(&disabled).is_empty());

    let mut enabled = BlockDevice::new(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::fifo("mock-sd", 8, true),
    );

    Interface::enable_irq(&mut enabled);
    assert!(Interface::is_irq_enabled(&enabled));
    Interface::disable_irq(&mut enabled);
    assert!(!Interface::is_irq_enabled(&enabled));
}

#[test]
fn irq_handler_does_not_enter_shared_card_core() {
    let mut device = BlockDevice::new(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::fifo("mock-sd", 8, true),
    );
    let mut handler = Interface::take_irq_handler(&mut device, 0).unwrap();
    let _guard = device.control.raw.inner.enter();

    let event = handler.handle_irq();

    assert!(event.queues.contains(0));
}

#[test]
fn dma_config_exposes_one_owned_queue_while_handle_is_live() {
    let dma = DeviceDma::new_identity(u32::MAX as u64, &TEST_DMA);
    let mut device = BlockDevice::new(
        SdioSdmmc::new(MockHost::default()),
        BlockConfig::dma("mock-sd", 8, false, dma),
    );

    assert!(Interface::create_queue(&mut device).is_none());
    let queue = Interface::create_owned_queue(&mut device);
    assert!(queue.is_some());
    assert!(Interface::create_owned_queue(&mut device).is_none());
    drop(queue);
    assert!(Interface::create_owned_queue(&mut device).is_some());
}

#[test]
fn poll_request_only_completes_matching_request_id() {
    let mut queue =
        BlockQueue::<MockHost>::new(block_control(BlockConfig::fifo("mock-sd", 8, false)), 0);
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
fn fifo_split_wrong_request_id_does_not_advance_child() {
    let control = block_control(
        BlockConfig::fifo("mock-sd", 16, false)
            .with_max_blocks_per_request(4)
            .with_max_segment_size(4 * BLOCK_SIZE),
    );
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let mut backing = [0u8; 4 * BLOCK_SIZE];
    let mut segments =
        [unsafe { rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len()) }];
    let request = Request {
        op: rdif_block::RequestOp::Read,
        lba: 0,
        block_count: 4,
        segments: &mut segments,
        flags: rdif_block::RequestFlags::NONE,
    };

    let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
    let wrong_id = RequestId::new(usize::from(id) + 1);

    assert_eq!(
        rdif_block::IQueue::poll_request(&mut queue, wrong_id),
        Ok(RequestStatus::Pending)
    );
    raw.with_mut(|raw| assert_eq!(raw.host().read_sizes, alloc::vec![BLOCK_SIZE]));

    let mut polls = 0;
    loop {
        polls += 1;
        match rdif_block::IQueue::poll_request(&mut queue, id) {
            Ok(RequestStatus::Pending) => {}
            Ok(RequestStatus::Complete) => break,
            Err(err) => panic!("split read poll failed: {err:?}"),
        }
    }
    assert_eq!(polls, 4);
    raw.with_mut(|raw| assert_eq!(raw.host().read_sizes, alloc::vec![BLOCK_SIZE; 4]));
}

#[test]
fn fifo_split_error_clears_state_and_allows_later_submit() {
    let control = block_control(
        BlockConfig::fifo("mock-sd", 16, false)
            .with_max_blocks_per_request(2)
            .with_max_segment_size(2 * BLOCK_SIZE),
    );
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let mut backing = [0u8; 2 * BLOCK_SIZE];
    let mut segments =
        [unsafe { rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len()) }];
    let request = Request {
        op: rdif_block::RequestOp::Read,
        lba: 0,
        block_count: 2,
        segments: &mut segments,
        flags: rdif_block::RequestFlags::NONE,
    };
    let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
    raw.with_mut(|raw| raw.host_mut().fail_next_poll.store(true, Ordering::Release));

    assert_eq!(
        rdif_block::IQueue::poll_request(&mut queue, id),
        Err(BlkError::Io)
    );
    assert!(queue.split_transfer.is_none());
    assert!(queue.pending.is_none());

    let mut retry_backing = [0u8; BLOCK_SIZE];
    let mut retry_segments = [unsafe {
        rdif_block::Buffer::from_raw_parts(retry_backing.as_mut_ptr(), 0, retry_backing.len())
    }];
    let retry = Request {
        op: rdif_block::RequestOp::Read,
        lba: 1,
        block_count: 1,
        segments: &mut retry_segments,
        flags: rdif_block::RequestFlags::NONE,
    };

    let retry_id = rdif_block::IQueue::submit_request(&mut queue, retry)
        .expect("split error should leave queue reusable");
    assert_eq!(
        rdif_block::IQueue::poll_request(&mut queue, retry_id),
        Ok(RequestStatus::Complete)
    );
}

#[test]
fn unsupported_ops_are_rejected() {
    let mut queue =
        BlockQueue::<MockHost>::new(block_control(BlockConfig::fifo("mock-sd", 8, false)), 0);
    let mut segments = [];
    let request = Request {
        op: rdif_block::RequestOp::Flush,
        lba: 0,
        block_count: 0,
        segments: &mut segments,
        flags: rdif_block::RequestFlags::NONE,
    };

    assert_eq!(
        rdif_block::IQueue::submit_request(&mut queue, request),
        Err(BlkError::NotSupported)
    );
}

#[test]
fn owned_dma_submit_error_returns_original_buffer() {
    let dma = DeviceDma::new_identity(u64::MAX, &TEST_DMA);
    let control = block_control(BlockConfig::dma("mock-sd", 8, false, dma));
    let raw = control.raw.clone();
    raw.with_mut(|raw| {
        raw.host_mut()
            .fail_owned_submit
            .store(true, Ordering::Release)
    });
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let buffer = prepared_dma(BLOCK_SIZE, dma_api::DmaDirection::ToDevice);
    let original_ptr = buffer.cpu_ptr();
    let request = OwnedRequest {
        op: rdif_block::RequestOp::Write,
        lba: 0,
        block_count: 1,
        data: Some(buffer),
        flags: rdif_block::RequestFlags::NONE,
    };

    let err = rdif_block::IQueueOwned::submit_request(&mut queue, request)
        .expect_err("unsupported owned op should return the request");
    assert_eq!(err.error, BlkError::Retry);
    let returned = err.request().data.as_ref().unwrap();
    assert_eq!(returned.cpu_ptr(), original_ptr);
    assert_eq!(returned.len().get(), BLOCK_SIZE);
}

#[test]
fn owned_dma_completion_is_returned_once() {
    let dma = DeviceDma::new_identity(u64::MAX, &TEST_DMA);
    let mut queue =
        BlockQueue::<MockHost>::new(block_control(BlockConfig::dma("mock-sd", 8, false, dma)), 0);
    let request = OwnedRequest {
        op: rdif_block::RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: Some(prepared_dma(BLOCK_SIZE, dma_api::DmaDirection::FromDevice)),
        flags: rdif_block::RequestFlags::NONE,
    };

    let id = match rdif_block::IQueueOwned::submit_request(&mut queue, request) {
        Ok(id) => id,
        Err(_) => panic!("owned DMA request should submit"),
    };
    let completion = match rdif_block::IQueueOwned::poll_request(&mut queue, id).unwrap() {
        OwnedRequestPoll::Ready(completion) => completion,
        OwnedRequestPoll::Pending => panic!("mock owned DMA request should complete"),
    };
    assert_eq!(completion.id, id);
    assert!(completion.result.is_ok());
    assert_eq!(completion.data.as_ref().unwrap().len().get(), BLOCK_SIZE);
    assert!(matches!(
        rdif_block::IQueueOwned::poll_request(&mut queue, id),
        Err(PollError::UnknownRequest)
    ));
}

#[test]
fn block_addr_for_card_bounds_standard_and_high_capacity_cards() {
    assert_eq!(block_addr_for_card(u32::MAX as u64, true), Ok(u32::MAX));
    assert_eq!(
        block_addr_for_card(u32::MAX as u64 + 1, true),
        Err(BlkError::InvalidBlockIndex(u32::MAX as u64 + 1))
    );

    let max_standard_block = u32::MAX as u64 / BLOCK_SIZE as u64;
    assert_eq!(
        block_addr_for_card(max_standard_block, false),
        Ok((max_standard_block * BLOCK_SIZE as u64) as u32)
    );
    assert_eq!(
        block_addr_for_card(max_standard_block + 1, false),
        Err(BlkError::InvalidBlockIndex(max_standard_block + 1))
    );
}

#[test]
fn dropping_queue_aborts_pending_request() {
    let control = block_control(BlockConfig::fifo("mock-sd", 8, false));
    let raw = control.raw.clone();
    let mut backing = [0u8; BLOCK_SIZE];
    let mut segments =
        [unsafe { rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len()) }];
    {
        let mut queue = BlockQueue::<MockHost>::new(Arc::clone(&control), 0);
        let request = Request {
            op: rdif_block::RequestOp::Read,
            lba: 0,
            block_count: 1,
            segments: &mut segments,
            flags: rdif_block::RequestFlags::NONE,
        };

        rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
    }

    assert_eq!(
        raw.with_mut(|raw| raw.host().aborts.load(Ordering::Acquire)),
        1
    );
}

#[test]
fn host2_submit_failure_does_not_leak_active_slot() {
    let mut host = SdioHost2Adapter::new(Host2BlockMock {
        submit_error: Some(sdio_host2::Error::Busy),
        ..Host2BlockMock::default()
    });
    let mut slot = ProtocolBlockSlot::default();
    let mut pending = None;
    let mut backing = [0u8; BLOCK_SIZE];
    let buffer = NonNull::new(backing.as_mut_ptr()).unwrap();
    let size = NonZeroUsize::new(BLOCK_SIZE).unwrap();

    assert_eq!(
        <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
            &mut host,
            0,
            buffer,
            size,
            None,
            &mut slot,
            &mut pending,
        ),
        Err(BlkError::Retry)
    );
    assert!(pending.is_none());
    assert!(slot.active_id.is_none());

    <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
        &mut host,
        0,
        buffer,
        size,
        None,
        &mut slot,
        &mut pending,
    )
    .expect("slot should accept a new request after submit failure");
}

#[test]
fn host2_poll_error_clears_pending_and_active_slot() {
    let mut host = SdioHost2Adapter::new(Host2BlockMock {
        poll_error: Some(sdio_host2::Error::Timeout),
        ..Host2BlockMock::default()
    });
    let mut slot = ProtocolBlockSlot::default();
    let mut pending = None;
    let mut backing = [0u8; BLOCK_SIZE];
    let buffer = NonNull::new(backing.as_mut_ptr()).unwrap();
    let size = NonZeroUsize::new(BLOCK_SIZE).unwrap();
    let id = <SdioHost2Adapter<Host2BlockMock> as BlockHost>::submit_read_request(
        &mut host,
        0,
        buffer,
        size,
        None,
        &mut slot,
        &mut pending,
    )
    .unwrap();

    assert!(matches!(
        <SdioHost2Adapter<Host2BlockMock> as BlockHost>::poll_block_request(
            &mut host,
            &mut pending,
            id,
            &mut slot,
        ),
        Err(Error::Timeout(_))
    ));
    assert!(pending.is_none());
    assert!(slot.active_id.is_none());
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
    fail_next_poll: AtomicBool,
    fail_owned_submit: AtomicBool,
    next_id: AtomicUsize,
    aborts: AtomicUsize,
    read_sizes: Vec<usize>,
    write_sizes: Vec<usize>,
}

struct MockIrqEndpoint;

impl SdioIrqHandle for MockIrqEndpoint {
    type Event = MockEvent;

    fn handle_irq(&mut self) -> Self::Event {
        MockEvent(HostEventKind::TransferComplete)
    }
}

#[derive(Default)]
struct MockSlot {
    in_flight: Option<dma_api::InFlightDma>,
    completed: Option<CompletedDma>,
}

struct MockRequest {
    id: BlockRequestId,
}

struct TestDma;
static TEST_DMA: TestDma = TestDma;

impl dma_api::DmaOp for TestDma {
    fn page_size(&self) -> usize {
        BLOCK_SIZE
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = unsafe { alloc_zeroed(layout) };
        NonNull::new(ptr).map(|ptr| unsafe {
            dma_api::DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: dma_api::DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        Err(dma_api::DmaError::NoMemory)
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

#[derive(Default)]
struct Host2BlockMock {
    submit_error: Option<sdio_host2::Error>,
    poll_error: Option<sdio_host2::Error>,
}

struct Host2BlockRequest {
    done: bool,
}

impl PhysicalSdioHost for Host2BlockMock {
    type TransactionRequest<'a>
        = Host2BlockRequest
    where
        Self: 'a;
    type BusRequest = Host2BlockRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        _transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        if let Some(err) = self.submit_error.take() {
            return Err(err);
        }
        Ok(Host2BlockRequest { done: false })
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        request.done = true;
        if let Some(err) = self.poll_error.take() {
            return Ok(RequestPoll::Ready(Err(err)));
        }
        Ok(RequestPoll::Ready(Ok(sdio_host2::RawResponse::empty())))
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        request.done = true;
        Ok(())
    }

    unsafe fn submit_bus_op(
        &mut self,
        _op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        Ok(Host2BlockRequest { done: false })
    }

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<RequestPoll<()>, sdio_host2::PollRequestError> {
        request.done = true;
        Ok(RequestPoll::Ready(Ok(())))
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        request.done = true;
        Ok(())
    }
}

impl SdioHost2Irq for Host2BlockMock {
    type Event = MockEvent;
    type IrqHandle = MockIrqEndpoint;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        MockIrqEndpoint
    }
}

impl SdioHost for MockHost {
    type Event = MockEvent;
    type DataRequest<'a> = ();
    type BusRequest = crate::sdio::ReadyBusRequest;

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

    fn submit_bus_op(&mut self, op: crate::sdio::SdioBusOp) -> Result<Self::BusRequest, Error> {
        crate::sdio::submit_ready_bus_op(self, op)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        crate::sdio::poll_ready_bus_op(request)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.irq_enabled.store(true, Ordering::Release);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.irq_enabled.store(false, Ordering::Release);
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }
}

impl SdioIrqHost for MockHost {
    type IrqHandle = MockIrqEndpoint;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        MockIrqEndpoint
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
        self.read_sizes.push(_size.get());
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
        self.write_sizes.push(_size.get());
        self.submit_mock_request(pending)
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        if self.fail_next_poll.swap(false, Ordering::AcqRel) {
            *pending = None;
            slot.in_flight = None;
            slot.completed = None;
            return Err(Error::BusError(Default::default()));
        }
        match pending.as_ref() {
            Some(active) if active.id == request => {
                *pending = None;
                if let Some(in_flight) = slot.in_flight.take() {
                    slot.completed = Some(unsafe { in_flight.complete_after_quiesce() });
                }
                Ok(BlockPoll::Complete)
            }
            Some(_) => Ok(BlockPoll::Pending),
            None => Ok(BlockPoll::Complete),
        }
    }

    fn abort_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        _slot: &mut Self::Slot,
    ) -> Result<(), Error> {
        if pending.take().is_some() {
            self.aborts.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        request.id
    }

    fn submit_owned_read_request(
        &mut self,
        _start_block: u32,
        buffer: PreparedDma,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        self.submit_mock_owned_request(buffer, slot, pending)
    }

    fn submit_owned_write_request(
        &mut self,
        _start_block: u32,
        buffer: PreparedDma,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        self.submit_mock_owned_request(buffer, slot, pending)
    }

    fn take_completed_dma(slot: &mut Self::Slot) -> Option<CompletedDma> {
        slot.completed.take()
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

    fn submit_mock_owned_request(
        &self,
        buffer: PreparedDma,
        slot: &mut MockSlot,
        pending: &mut Option<MockRequest>,
    ) -> Result<BlockRequestId, OwnedBlockSubmitError> {
        if self.fail_owned_submit.swap(false, Ordering::AcqRel) {
            return Err(OwnedBlockSubmitError::new(BlkError::Retry, buffer));
        }
        match self.submit_mock_request(pending) {
            Ok(id) => {
                slot.in_flight = Some(unsafe { buffer.into_in_flight() });
                Ok(id)
            }
            Err(err) => Err(OwnedBlockSubmitError::new(err, buffer)),
        }
    }
}

fn prepared_dma(size: usize, direction: dma_api::DmaDirection) -> PreparedDma {
    let dma = DeviceDma::new_identity(u64::MAX, &TEST_DMA);
    dma_api::CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(size).expect("test DMA allocation must be non-zero"),
        BLOCK_SIZE,
        direction,
    )
    .expect("test DMA allocation should succeed")
    .prepare_for_device()
}

#[test]
fn fifo_read_requests_are_split_to_single_blocks() {
    let control = block_control(
        BlockConfig::fifo("mock-sd", 16, false)
            .with_max_blocks_per_request(8)
            .with_max_segment_size(8 * BLOCK_SIZE),
    );
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let mut backing = [0u8; 8 * BLOCK_SIZE];
    let mut segments =
        [unsafe { rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len()) }];
    let request = Request {
        op: rdif_block::RequestOp::Read,
        lba: 0,
        block_count: 8,
        segments: &mut segments,
        flags: rdif_block::RequestFlags::NONE,
    };

    let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
    let mut polls = 0;
    loop {
        polls += 1;
        match rdif_block::IQueue::poll_request(&mut queue, id) {
            Ok(RequestStatus::Pending) => {}
            Ok(RequestStatus::Complete) => break,
            Err(err) => panic!("split read poll failed: {err:?}"),
        }
    }
    assert_eq!(polls, 8);

    raw.with_mut(|raw| {
        assert_eq!(
            raw.host().read_sizes,
            alloc::vec![BLOCK_SIZE; 8],
            "FIFO read should avoid CMD18 multi-block requests on hosts without DMA"
        );
        assert!(raw.host().write_sizes.is_empty());
    });
}

#[test]
fn fifo_write_requests_are_split_to_single_blocks() {
    let control = block_control(
        BlockConfig::fifo("mock-sd", 16, false)
            .with_max_blocks_per_request(8)
            .with_max_segment_size(8 * BLOCK_SIZE),
    );
    let raw = control.raw.clone();
    let mut queue = BlockQueue::<MockHost>::new(control, 0);
    let mut backing = [0u8; 8 * BLOCK_SIZE];
    let mut segments =
        [unsafe { rdif_block::Buffer::from_raw_parts(backing.as_mut_ptr(), 0, backing.len()) }];
    let request = Request {
        op: rdif_block::RequestOp::Write,
        lba: 0,
        block_count: 8,
        segments: &mut segments,
        flags: rdif_block::RequestFlags::NONE,
    };

    let id = rdif_block::IQueue::submit_request(&mut queue, request).unwrap();
    let mut polls = 0;
    loop {
        polls += 1;
        match rdif_block::IQueue::poll_request(&mut queue, id) {
            Ok(RequestStatus::Pending) => {}
            Ok(RequestStatus::Complete) => break,
            Err(err) => panic!("split write poll failed: {err:?}"),
        }
    }
    assert_eq!(polls, 8);

    raw.with_mut(|raw| {
        assert!(raw.host().read_sizes.is_empty());
        assert_eq!(
            raw.host().write_sizes,
            alloc::vec![BLOCK_SIZE; 8],
            "FIFO write should avoid CMD25/CMD12 multi-block requests on hosts without DMA"
        );
    });
}
