use alloc::{boxed::Box, vec, vec::Vec};

use axtest::prelude::*;
use rdif_base::DriverGeneric;

use crate::{
    BlkError, CompletionHint, CompletionIds, CompletionList, CompletionSink, DeviceInfo, Event,
    IQueue, IQueueOwned, IdList, Interface, IrqSourceInfo, MAX_COMPLETION_HINTS, OwnedRequest,
    PollError, QueueHandle, QueueInfo, QueueLimits, Request, RequestFlags, RequestId, RequestOp,
    RequestPoll, RequestStatus, Segment, SubmitError, TransferPlanner, TransferRuntimeCaps,
    validate_owned_request_shape, validate_request, validate_request_shape,
};

fn queue_info_with(limits: QueueLimits) -> QueueInfo {
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(64, 512),
        limits,
    }
}

fn segment_from(bytes: &mut [u8], bus: u64) -> Segment<'_> {
    unsafe { Segment::from_raw_parts(bytes.as_mut_ptr(), bus, bytes.len()) }
}

#[axtest]
fn rdif_block_device_queue_info_and_error_mapping_rules_hold() {
    let mut device = DeviceInfo::new(128, 512);
    device.read_only = true;
    device.name = Some("vda");
    device.vendor = Some("virtio");
    device.model = Some("blk");

    let limits = QueueLimits::simple(512, 0xffff_ffff);
    let info = QueueInfo {
        id: 3,
        device,
        limits,
    };

    ax_assert_eq!(info.id, 3);
    ax_assert_eq!(info.device.num_blocks, 128);
    ax_assert!(info.device.read_only);
    ax_assert_eq!(info.limits.dma_alignment, 512);
    ax_assert_eq!(info.limits.max_inflight, 1);
    ax_assert_eq!(info.limits.max_segment_size, 512);

    ax_assert_eq!(
        alloc::format!("{}", BlkError::InvalidBlockIndex(9)),
        "invalid block index: 9"
    );
    ax_assert_eq!(
        alloc::format!("{}", BlkError::NotSupported),
        "operation not supported"
    );
    ax_assert_eq!(
        alloc::format!("{}", BlkError::Retry),
        "operation should be retried"
    );
    ax_assert_eq!(
        alloc::format!("{}", BlkError::NoMemory),
        "insufficient memory"
    );
    ax_assert_eq!(
        alloc::format!("{}", BlkError::InvalidRequest),
        "invalid block request"
    );
    ax_assert_eq!(alloc::format!("{}", BlkError::Io), "block I/O error");
    ax_assert_eq!(alloc::format!("{}", BlkError::Other("custom")), "custom");
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::NotSupported),
        crate::io::ErrorKind::Unsupported
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::Retry),
        crate::io::ErrorKind::Interrupted
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::NoMemory),
        crate::io::ErrorKind::OutOfMemory
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::InvalidRequest),
        crate::io::ErrorKind::InvalidParameter {
            name: "block request"
        }
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::Io),
        crate::io::ErrorKind::Other(_)
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::InvalidBlockIndex(17)),
        crate::io::ErrorKind::NotAvailable
    ));
    ax_assert!(matches!(
        crate::io::ErrorKind::from(BlkError::Other("custom block error")),
        crate::io::ErrorKind::Other(_)
    ));
    ax_assert_eq!(
        BlkError::from(dma_api::DmaError::NoMemory),
        BlkError::NoMemory
    );
    ax_assert_eq!(
        BlkError::from(dma_api::DmaError::SegmentTooLarge { size: 2, max: 1 }),
        BlkError::Io
    );
}

#[axtest]
fn rdif_block_request_flags_ids_segments_and_submit_error_round_trip() {
    let id = RequestId::new(12);
    ax_assert_eq!(usize::from(id), 12);
    ax_assert_eq!(RequestStatus::Pending, RequestStatus::Pending);
    ax_assert_ne!(RequestStatus::Pending, RequestStatus::Complete);

    let flags = RequestFlags::FUA | RequestFlags::SYNC;
    ax_assert!(flags.contains(RequestFlags::FUA));
    ax_assert!(flags.intersects(RequestFlags::SYNC));
    ax_assert_eq!(
        flags.unsupported_by(RequestFlags::FUA).bits(),
        RequestFlags::SYNC.bits()
    );
    let mut assigned = RequestFlags::NONE;
    assigned |= RequestFlags::NOWAIT;
    ax_assert_eq!(assigned.bits(), RequestFlags::NOWAIT.bits());

    let mut bytes = [0x5a_u8; 4];
    let mut segment = segment_from(&mut bytes, 0x1000);
    ax_assert_eq!(segment.bus, 0x1000);
    ax_assert_eq!(&*segment, &[0x5a; 4]);
    segment[0] = 0xa5;
    ax_assert_eq!(bytes[0], 0xa5);

    let request = OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    };
    let error = SubmitError::new(BlkError::Retry, request);
    ax_assert_eq!(error.error, BlkError::Retry);
    ax_assert_eq!(error.request().op, RequestOp::Flush);
    ax_assert_eq!(error.into_request().block_count, 0);
}

#[axtest]
fn rdif_block_request_validation_accepts_data_ops_and_rejects_bad_shapes() {
    let info = DeviceInfo::new(8, 512);
    let limits = QueueLimits {
        max_blocks_per_request: 8,
        max_segment_size: 1024,
        max_segments: 2,
        ..QueueLimits::simple(512, u64::MAX)
    };
    let mut bytes = [0_u8; 1024];
    let segment = segment_from(&mut bytes, 0x1000);
    let mut segments = [segment];
    let request = Request {
        op: RequestOp::Read,
        lba: 1,
        block_count: 2,
        segments: &mut segments,
        flags: RequestFlags::NONE,
    };

    ax_assert_eq!(request.data_len(), 1024);
    ax_assert!(request.is_data_op());
    ax_assert_eq!(validate_request_shape(info, limits, &request), Ok(()));
    ax_assert_eq!(validate_request(queue_info_with(limits), &request), Ok(()));

    let mut short = [0_u8; 512];
    let segment = segment_from(&mut short, 0x2000);
    let mut segments = [segment];
    let bad_len = Request {
        op: RequestOp::Write,
        lba: 1,
        block_count: 2,
        segments: &mut segments,
        flags: RequestFlags::NONE,
    };
    ax_assert_eq!(
        validate_request_shape(info, limits, &bad_len),
        Err(BlkError::InvalidRequest)
    );

    let mut empty_segments = [];
    let bad_lba = Request {
        op: RequestOp::Discard,
        lba: 8,
        block_count: 1,
        segments: &mut empty_segments,
        flags: RequestFlags::NONE,
    };
    ax_assert_eq!(
        validate_request_shape(info, limits, &bad_lba),
        Err(BlkError::InvalidBlockIndex(8))
    );
}

#[axtest]
fn rdif_block_request_validation_handles_flush_discard_write_zeroes_and_flags() {
    let info = DeviceInfo::new(64, 512);
    let mut limits = QueueLimits {
        max_blocks_per_request: 8,
        supports_flush: true,
        supports_discard: true,
        supports_write_zeroes: true,
        supported_flags: RequestFlags::FUA | RequestFlags::PREFLUSH,
        ..QueueLimits::simple(512, u64::MAX)
    };

    for op in [RequestOp::Flush, RequestOp::Discard, RequestOp::WriteZeroes] {
        let mut segments = [];
        let request = Request {
            op,
            lba: 0,
            block_count: if matches!(op, RequestOp::Flush) { 0 } else { 1 },
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };
        ax_assert_eq!(validate_request_shape(info, limits, &request), Ok(()));
    }

    limits.supports_flush = false;
    let info_with_limits = queue_info_with(limits);
    let mut bytes = [0_u8; 512];
    let segment = segment_from(&mut bytes, 0x1000);
    let mut segments = [segment];
    let preflush = Request {
        op: RequestOp::Write,
        lba: 0,
        block_count: 1,
        segments: &mut segments,
        flags: RequestFlags::PREFLUSH,
    };
    ax_assert_eq!(
        validate_request(info_with_limits, &preflush),
        Err(BlkError::NotSupported)
    );

    let unknown_flags = RequestFlags::from_bits_for_test(1 << 24);
    let mut segments = [segment_from(&mut bytes, 0x2000)];
    let bad_flags = Request {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        segments: &mut segments,
        flags: unknown_flags,
    };
    ax_assert_eq!(
        validate_request(
            queue_info_with(QueueLimits::simple(512, u64::MAX)),
            &bad_flags
        ),
        Err(BlkError::InvalidRequest)
    );
}

#[axtest]
fn rdif_block_owned_request_validation_covers_control_ops_without_dma() {
    let info = DeviceInfo::new(64, 512);
    let limits = QueueLimits {
        max_blocks_per_request: 8,
        supports_flush: true,
        supports_discard: true,
        supports_write_zeroes: true,
        ..QueueLimits::simple(512, u64::MAX)
    };

    for op in [RequestOp::Flush, RequestOp::Discard, RequestOp::WriteZeroes] {
        let request = OwnedRequest {
            op,
            lba: 0,
            block_count: if matches!(op, RequestOp::Flush) { 0 } else { 1 },
            data: None,
            flags: RequestFlags::NONE,
        };
        ax_assert!(!request.is_data_op());
        ax_assert_eq!(request.data_len(), 0);
        ax_assert_eq!(validate_owned_request_shape(info, limits, &request), Ok(()));
    }

    let bad = OwnedRequest {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: None,
        flags: RequestFlags::NONE,
    };
    ax_assert_eq!(
        validate_owned_request_shape(info, limits, &bad),
        Err(BlkError::InvalidRequest)
    );
}

#[axtest]
fn rdif_block_irq_lists_completion_batches_and_events_hold() {
    let mut queues = IdList::none();
    queues.insert(2);
    queues.insert(63);
    queues.insert(64);
    ax_assert!(queues.contains(2));
    ax_assert!(queues.contains(63));
    ax_assert!(!queues.contains(64));
    queues.remove(2);
    ax_assert!(!queues.contains(2));
    ax_assert_eq!(queues.iter().collect::<Vec<_>>(), vec![63]);

    let source = IrqSourceInfo::legacy(queues);
    ax_assert_eq!(source.id, 0);
    ax_assert_eq!(IrqSourceInfo::new(7, queues).id, 7);

    let mut ids = CompletionIds::new();
    ax_assert!(ids.is_empty());
    for id in 0..crate::MAX_BATCH_COMPLETION_IDS {
        ax_assert!(ids.push(RequestId::new(id)));
    }
    ax_assert!(!ids.push(RequestId::new(99)));
    ax_assert_eq!(ids.len(), crate::MAX_BATCH_COMPLETION_IDS);
    ax_assert_eq!(ids.iter().next(), Some(RequestId::new(0)));

    let batch = CompletionHint::Batch { queue_id: 5, ids };
    ax_assert_eq!(batch.queue_id(), 5);

    let mut list = CompletionList::new();
    for idx in 0..MAX_COMPLETION_HINTS {
        ax_assert!(list.push(CompletionHint::Request {
            queue_id: 1,
            request_id: RequestId::new(idx)
        }));
    }
    ax_assert!(!list.push(CompletionHint::Queue { queue_id: 1 }));
    ax_assert_eq!(list.len(), MAX_COMPLETION_HINTS);

    let mut event = Event::from_hint(CompletionHint::Queue { queue_id: 3 });
    ax_assert!(event.queues.contains(3));
    event.push_request(4, RequestId::new(1));
    event.push_hint(batch);
    ax_assert!(!event.is_empty());
    ax_assert!(Event::from_queue_bits(1 << 7).queues.contains(7));
}

#[derive(Default)]
struct RecordingSink {
    completions: Vec<(RequestId, Result<(), BlkError>)>,
}

impl CompletionSink for RecordingSink {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>) {
        self.completions.push((request_id, result));
    }
}

struct BatchQueue;

unsafe impl IQueue for BatchQueue {
    fn id(&self) -> usize {
        1
    }

    fn info(&self) -> QueueInfo {
        queue_info_with(QueueLimits::simple(512, u64::MAX))
    }

    fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
        Ok(RequestId::new(1))
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        match usize::from(request) {
            1 => Ok(RequestStatus::Complete),
            2 => Ok(RequestStatus::Pending),
            3 => Err(BlkError::Io),
            _ => Err(BlkError::InvalidRequest),
        }
    }
}

#[axtest]
fn rdif_block_queue_default_completion_poll_reports_terminal_results() {
    let mut queue = BatchQueue;
    ax_assert_eq!(queue.id(), 1);
    ax_assert_eq!(queue.info().device.logical_block_size, 512);

    let mut sink = RecordingSink::default();
    let ids = [RequestId::new(1), RequestId::new(2), RequestId::new(3)];
    queue.poll_completions(&ids, &mut sink).unwrap();
    ax_assert_eq!(
        sink.completions,
        vec![
            (RequestId::new(1), Ok(())),
            (RequestId::new(3), Err(BlkError::Io))
        ]
    );
}

struct OwnedQueue {
    shutdowns: usize,
}

impl IQueueOwned for OwnedQueue {
    fn id(&self) -> usize {
        2
    }

    fn info(&self) -> QueueInfo {
        queue_info_with(QueueLimits::simple(512, u64::MAX))
    }

    fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        if matches!(request.op, RequestOp::Flush) {
            Ok(RequestId::new(9))
        } else {
            Err(SubmitError::new(BlkError::InvalidRequest, request))
        }
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
        if request == RequestId::new(9) {
            Ok(RequestPoll::Pending)
        } else {
            Err(PollError::UnknownRequest)
        }
    }

    fn cancel_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
        if request == RequestId::new(9) {
            Ok(RequestPoll::Ready(crate::CompletedRequest::new(
                request,
                Ok(()),
                None,
            )))
        } else {
            Err(PollError::WrongQueue)
        }
    }

    fn shutdown(&mut self) {
        self.shutdowns += 1;
    }
}

#[axtest]
fn rdif_block_owned_queue_handle_delegates_and_returns_request_on_submit_error() {
    let mut handle = QueueHandle::new(Box::new(OwnedQueue { shutdowns: 0 }));
    ax_assert_eq!(handle.id(), 2);
    ax_assert_eq!(handle.info().id, 0);

    let flush = OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    };
    match handle.submit_request(flush) {
        Ok(id) => ax_assert_eq!(id, RequestId::new(9)),
        Err(_) => panic!("flush request should be accepted"),
    }
    ax_assert!(matches!(
        handle.poll_request(RequestId::new(9)),
        Ok(RequestPoll::Pending)
    ));
    ax_assert!(matches!(
        handle.cancel_request(RequestId::new(9)),
        Ok(RequestPoll::Ready(_))
    ));

    let bad = OwnedRequest {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: None,
        flags: RequestFlags::NONE,
    };
    let error = handle.submit_request(bad).unwrap_err();
    ax_assert_eq!(error.error, BlkError::InvalidRequest);
    ax_assert_eq!(error.into_request().op, RequestOp::Read);
    handle.shutdown();
}

struct MinimalBlock {
    irq_enabled: bool,
}

impl crate::DriverGeneric for MinimalBlock {
    fn name(&self) -> &str {
        "minimal-block"
    }
}

impl Interface for MinimalBlock {
    fn device_info(&self) -> DeviceInfo {
        DeviceInfo::new(16, 512)
    }

    fn queue_limits(&self) -> QueueLimits {
        QueueLimits::simple(512, u64::MAX)
    }

    fn create_queue(&mut self) -> Option<crate::BQueue> {
        Some(Box::new(BatchQueue))
    }

    fn enable_irq(&self) {}

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }
}

#[axtest]
fn rdif_block_interface_defaults_and_boxed_queue_types_hold() {
    let mut device = MinimalBlock { irq_enabled: true };
    ax_assert_eq!(device.name(), "minimal-block");
    ax_assert_eq!(device.device_info().num_blocks, 16);
    ax_assert_eq!(device.queue_limits().dma_alignment, 512);
    ax_assert!(device.create_queue().is_some());
    ax_assert!(device.create_owned_queue().is_none());
    ax_assert!(device.irq_sources().is_empty());
    ax_assert!(device.take_irq_handler(0).is_none());
    ax_assert!(device.is_irq_enabled());
    device.disable_irq();
}

#[axtest]
fn rdif_block_transfer_planner_splits_chunks_and_segments() {
    let device = DeviceInfo::new(64, 512);
    let limits = QueueLimits {
        max_blocks_per_request: 4,
        max_segments: 2,
        max_segment_size: 512,
        ..QueueLimits::simple(512, u64::MAX)
    };
    let caps = TransferRuntimeCaps::new(4096, 2);
    let planner = TransferPlanner::new(device, limits, caps).unwrap();
    ax_assert_eq!(planner.chunk_size(), 1024);

    let mut plan = planner.plan_from(2, 2048, 128).unwrap();
    let first = plan.next().unwrap();
    ax_assert_eq!(first.lba, 2);
    ax_assert_eq!(first.block_count, 2);
    ax_assert_eq!(first.byte_offset, 128);
    ax_assert_eq!(first.byte_len, 1024);
    ax_assert_eq!(
        first.segments().collect::<Vec<_>>(),
        vec![
            crate::TransferSegment {
                byte_offset: 0,
                byte_len: 512
            },
            crate::TransferSegment {
                byte_offset: 512,
                byte_len: 512
            }
        ]
    );
    ax_assert_eq!(plan.next().unwrap().lba, 4);
    ax_assert!(plan.next().is_none());

    ax_assert!(matches!(
        planner.plan(0, 513),
        Err(BlkError::InvalidRequest)
    ));
    ax_assert!(matches!(
        TransferPlanner::new(
            DeviceInfo::new(64, 0),
            QueueLimits::simple(512, u64::MAX),
            caps
        ),
        Err(BlkError::InvalidRequest)
    ));
}
