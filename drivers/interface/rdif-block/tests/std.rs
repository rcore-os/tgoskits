extern crate alloc;

use alloc::{vec, vec::Vec};

use rdif_block::{
    BlkError, DeviceInfo, OwnedRequest, QueueInfo, QueueLimits, RequestFlags, RequestOp,
    TransferPlanner, TransferRuntimeCaps, validate_owned_request, validate_owned_request_shape,
};

fn dma_info(mask: u64, coherency: dma_api::DmaCoherency) -> dma_api::DmaDeviceInfo {
    dma_api::DmaDeviceInfo::new(
        dma_api::DmaDomainId::Direct,
        coherency,
        dma_api::DmaConstraints::new(mask),
    )
}

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
fn rdif_block_errors_map_to_io_kinds() {
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
fn rdif_block_owned_request_validation_rejects_invalid_shapes_and_flags() {
    let info = DeviceInfo::new(64, 512);
    let limits = QueueLimits {
        max_blocks_per_request: 8,
        supports_flush: true,
        supported_flags: RequestFlags::FUA | RequestFlags::PREFLUSH,
        ..QueueLimits::simple(512, dma_info(u64::MAX, dma_api::DmaCoherency::NonCoherent))
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
            queue_info_with(QueueLimits::simple(
                512,
                dma_info(u64::MAX, dma_api::DmaCoherency::NonCoherent),
            )),
            &unsupported_preflush
        ),
        Err(BlkError::NotSupported)
    );
}

#[test]
fn rdif_block_transfer_planner_splits_chunks_and_segments() {
    let device = DeviceInfo::new(64, 512);
    let limits = QueueLimits {
        dma: dma_info(u64::MAX, dma_api::DmaCoherency::NonCoherent).with_constraints(
            dma_api::DmaConstraints::new(u64::MAX)
                .with_align(512)
                .with_max_segment_size(512),
        ),
        max_blocks_per_request: 4,
        max_segments: 2,
        ..QueueLimits::simple(512, dma_info(u64::MAX, dma_api::DmaCoherency::NonCoherent))
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
            QueueLimits::simple(512, dma_info(u64::MAX, dma_api::DmaCoherency::NonCoherent),),
            caps
        ),
        Err(BlkError::InvalidRequest)
    ));
}
