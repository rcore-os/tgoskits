extern crate alloc;

use alloc::{vec, vec::Vec};

use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, CompletedRequest, CompletionSink,
    ControlEvent, DeviceInfo, HardwareQueue, IrqAck, IrqDisposition, IrqQueueMask, OwnedRequest,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestFlags, RequestId, RequestOp, SubmissionSink,
    SubmitError, TransferPlanner, TransferRuntimeCaps, validate_owned_request,
    validate_owned_request_shape,
};

fn queue_info_with(limits: QueueLimits) -> QueueInfo {
    QueueInfo {
        id: 0,
        device: DeviceInfo::new(64, 512),
        limits,
    }
}

fn flush_request() -> OwnedRequest {
    OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags: RequestFlags::NONE,
    }
}

#[test]
fn rdif_block_device_queue_info_and_error_mapping_rules_hold() {
    let mut device = DeviceInfo::new(128, 512);
    device.read_only = true;
    device.name = Some("nvme0n1");
    device.vendor = Some("qemu");
    device.model = Some("nvme");

    let limits = QueueLimits::simple(512, 0xffff_ffff);
    let info = QueueInfo {
        id: 3,
        device,
        limits,
    };

    assert_eq!(info.id, 3);
    assert_eq!(info.device.num_blocks, 128);
    assert!(info.device.read_only);
    assert_eq!(info.limits.dma_alignment, 512);
    assert_eq!(info.limits.max_inflight, 1);
    assert_eq!(info.limits.max_submit_batch, 1);
    assert_eq!(info.limits.max_segment_size, 512);

    assert_eq!(
        alloc::format!("{}", BlkError::InvalidBlockIndex(9)),
        "invalid block index: 9"
    );
    assert_eq!(
        alloc::format!("{}", BlkError::NotSupported),
        "operation not supported"
    );
    assert_eq!(
        alloc::format!("{}", BlkError::Retry),
        "operation should be retried"
    );
    assert_eq!(
        alloc::format!("{}", BlkError::NoMemory),
        "insufficient memory"
    );
    assert_eq!(
        alloc::format!("{}", BlkError::InvalidRequest),
        "invalid block request"
    );
    assert_eq!(alloc::format!("{}", BlkError::Io), "block I/O error");
    assert_eq!(alloc::format!("{}", BlkError::Other("custom")), "custom");
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::NotSupported),
        rdif_block::io::ErrorKind::Unsupported
    ));
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::Retry),
        rdif_block::io::ErrorKind::Interrupted
    ));
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::NoMemory),
        rdif_block::io::ErrorKind::OutOfMemory
    ));
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::InvalidRequest),
        rdif_block::io::ErrorKind::InvalidParameter {
            name: "block request"
        }
    ));
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::Io),
        rdif_block::io::ErrorKind::Other(_)
    ));
    assert!(matches!(
        rdif_block::io::ErrorKind::from(BlkError::InvalidBlockIndex(17)),
        rdif_block::io::ErrorKind::NotAvailable
    ));
    assert_eq!(
        BlkError::from(dma_api::DmaError::NoMemory),
        BlkError::NoMemory
    );
    assert_eq!(
        BlkError::from(dma_api::DmaError::SegmentTooLarge { size: 2, max: 1 }),
        BlkError::Io
    );
}

#[test]
fn rdif_block_request_flags_ids_and_submit_error_round_trip() {
    let id = RequestId::new(12);
    assert_eq!(usize::from(id), 12);

    let flags = RequestFlags::FUA | RequestFlags::PREFLUSH;
    assert!(flags.contains(RequestFlags::FUA));
    assert!(flags.intersects(RequestFlags::PREFLUSH));
    assert_eq!(
        flags.unsupported_by(RequestFlags::FUA).bits(),
        RequestFlags::PREFLUSH.bits()
    );
    let mut assigned = RequestFlags::NONE;
    assigned |= RequestFlags::NOWAIT;
    assert_eq!(assigned.bits(), RequestFlags::NOWAIT.bits());

    let request = flush_request();
    let error = SubmitError::new(BlkError::Retry, request);
    assert_eq!(error.error, BlkError::Retry);
    assert_eq!(error.request().op, RequestOp::Flush);
    assert_eq!(error.into_request().block_count, 0);
}

#[test]
fn rdif_block_owned_request_validation_rejects_invalid_shapes_and_flags() {
    let info = DeviceInfo::new(64, 512);
    let limits = QueueLimits {
        max_blocks_per_request: 8,
        supports_flush: true,
        supported_flags: RequestFlags::FUA | RequestFlags::PREFLUSH,
        ..QueueLimits::simple(512, u64::MAX)
    };

    let flush = flush_request();
    assert_eq!(validate_owned_request_shape(info, limits, &flush), Ok(()));
    assert_eq!(
        validate_owned_request(queue_info_with(limits), &flush),
        Ok(())
    );

    let missing_dma = OwnedRequest {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: None,
        flags: RequestFlags::NONE,
    };
    assert_eq!(
        validate_owned_request_shape(info, limits, &missing_dma),
        Err(BlkError::InvalidRequest)
    );

    let malformed_flush = OwnedRequest {
        block_count: 1,
        ..flush_request()
    };
    assert_eq!(
        validate_owned_request_shape(info, limits, &malformed_flush),
        Err(BlkError::InvalidRequest)
    );

    let unsupported_preflush = OwnedRequest {
        flags: RequestFlags::PREFLUSH,
        ..flush_request()
    };
    assert_eq!(
        validate_owned_request(
            queue_info_with(QueueLimits::simple(512, u64::MAX)),
            &unsupported_preflush
        ),
        Err(BlkError::NotSupported)
    );
}

#[test]
fn rdif_block_irq_ack_carries_fixed_queue_and_control_events() {
    let mut queues = IrqQueueMask::from_queue(2);
    assert!(queues.contains(2));
    assert!(!queues.contains(64));
    queues = IrqQueueMask::from_bits(queues.bits() | (1 << 7));
    assert!(queues.contains(7));

    let ack = IrqAck::cleared(queues, ControlEvent::new(5, 0x20));
    assert_eq!(ack.disposition(), IrqDisposition::Cleared);
    assert_eq!(ack.queues().bits(), (1 << 2) | (1 << 7));
    assert_eq!(ack.control_event().source_id(), 5);
    assert_eq!(ack.control_event().bits(), 0x20);
    assert!(IrqAck::spurious(5).is_spurious());
}

#[derive(Default)]
struct AcceptedIds(Vec<RequestId>);

impl SubmissionSink for AcceptedIds {
    fn accepted(&mut self, id: RequestId) {
        self.0.push(id);
    }
}

#[derive(Default)]
struct RecordingSink {
    completions: Vec<(RequestId, Result<(), BlkError>)>,
}

impl CompletionSink for RecordingSink {
    fn complete(&mut self, request: CompletedRequest) {
        assert!(request.data.is_none());
        self.completions.push((request.id, request.result));
    }
}

#[derive(Default)]
struct BatchQueue {
    next_id: usize,
    pending: Vec<RequestId>,
    commits: usize,
}

impl HardwareQueue for BatchQueue {
    fn id(&self) -> usize {
        1
    }

    fn info(&self) -> QueueInfo {
        let limits = QueueLimits {
            supports_flush: true,
            max_inflight: 2,
            max_submit_batch: 2,
            ..QueueLimits::simple(512, u64::MAX)
        };
        queue_info_with(limits)
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let Some(request) = requests.pop_front() else {
            return BatchSubmitResult::new(0, BatchSubmitDisposition::Continue);
        };
        assert_eq!(request.op, RequestOp::Flush);
        let id = RequestId::new(self.next_id);
        self.next_id += 1;
        self.pending.push(id);
        sink.accepted(id);
        let disposition = if requests.is_empty() {
            BatchSubmitDisposition::Continue
        } else {
            BatchSubmitDisposition::QueueFull
        };
        BatchSubmitResult::new(1, disposition)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.commits += 1;
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for id in self.pending.drain(..) {
            sink.complete(CompletedRequest::new(id, Ok(()), None));
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for id in self.pending.drain(..) {
            sink.complete(CompletedRequest::new(id, Err(BlkError::Io), None));
        }
        Ok(())
    }
}

#[test]
fn rdif_block_hardware_queue_batches_commit_and_return_ownership() {
    let mut queue = BatchQueue::default();
    let mut batch = OwnedRequestBatch::from_iter([flush_request(), flush_request()]);
    let mut accepted = AcceptedIds::default();

    let result = queue.submit_batch_owned(&mut batch, &mut accepted);
    assert_eq!(result.accepted(), 1);
    assert_eq!(result.disposition(), BatchSubmitDisposition::QueueFull);
    assert_eq!(batch.len(), 1);
    assert_eq!(accepted.0, vec![RequestId::new(0)]);
    assert_eq!(queue.commits, 0);

    queue.commit_submissions().unwrap();
    assert_eq!(queue.commits, 1);

    let mut completed = RecordingSink::default();
    queue.drain_completions(&mut completed).unwrap();
    assert_eq!(completed.completions, vec![(RequestId::new(0), Ok(()))]);
    assert!(queue.pending.is_empty());
}

#[test]
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
    assert_eq!(planner.chunk_size(), 1024);

    let mut plan = planner.plan_from(2, 2048, 128).unwrap();
    let first = plan.next().unwrap();
    assert_eq!(first.lba, 2);
    assert_eq!(first.block_count, 2);
    assert_eq!(first.byte_offset, 128);
    assert_eq!(first.byte_len, 1024);
    assert_eq!(
        first.segments().collect::<Vec<_>>(),
        vec![
            rdif_block::TransferSegment {
                byte_offset: 0,
                byte_len: 512
            },
            rdif_block::TransferSegment {
                byte_offset: 512,
                byte_len: 512
            }
        ]
    );
    assert_eq!(plan.next().unwrap().lba, 4);
    assert!(plan.next().is_none());

    assert!(matches!(
        planner.plan(0, 513),
        Err(BlkError::InvalidRequest)
    ));
    assert!(matches!(
        TransferPlanner::new(
            DeviceInfo::new(64, 0),
            QueueLimits::simple(512, u64::MAX),
            caps
        ),
        Err(BlkError::InvalidRequest)
    ));
}
