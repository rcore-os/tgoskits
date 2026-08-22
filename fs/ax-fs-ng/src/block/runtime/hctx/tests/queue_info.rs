use super::*;

fn changed_queue_info(mut info: QueueInfo, change: impl FnOnce(&mut QueueInfo)) -> QueueInfo {
    change(&mut info);
    info
}

#[test]
fn provisional_queue_info_rejects_zero_submit_batch() {
    let observed = changed_queue_info(test_queue_info(8), |info| {
        info.limits.max_submit_batch = 0;
    });

    assert!(!queue_info_fits_provisioned(test_queue_info(8), observed));
}

#[test]
fn provisional_queue_info_rejects_zero_depth_and_submit_batch() {
    let observed = changed_queue_info(test_queue_info(8), |info| {
        info.limits.max_inflight = 0;
        info.limits.max_submit_batch = 0;
    });

    assert!(!queue_info_fits_provisioned(test_queue_info(8), observed));
}

#[test]
fn provisional_queue_info_accepts_compatible_discovery_updates() {
    let baseline = test_queue_info(8);
    let mut discovered = changed_queue_info(baseline, |info| {
        info.device.num_blocks = 4096;
        info.device.name = Some("discovered-device");
        info.limits.max_inflight = 4;
        info.limits.max_submit_batch = 2;
    });
    let mut epoch = QueueInfoEpoch::new(baseline);

    assert_eq!(epoch.observe(discovered), Ok(()));
    assert_eq!(epoch.published(), discovered);

    discovered.limits.max_inflight = 2;
    discovered.limits.max_submit_batch = 1;
    assert_eq!(epoch.observe(discovered), Ok(()));
    assert_eq!(epoch.published(), discovered);
}

#[test]
fn provisional_queue_info_rejects_identity_and_reserved_capacity_growth() {
    let baseline = test_queue_info(8);
    let changes = [
        changed_queue_info(baseline, |info| info.id += 1),
        changed_queue_info(baseline, |info| info.limits.max_inflight += 1),
        changed_queue_info(baseline, |info| info.limits.max_submit_batch += 1),
        changed_queue_info(baseline, |info| {
            info.limits.max_inflight = 4;
            info.limits.max_submit_batch = 5;
        }),
    ];

    for observed in changes {
        let mut epoch = QueueInfoEpoch::new(baseline);
        assert_eq!(epoch.observe(observed), Err(BlkError::InvalidRequest));
        assert_eq!(epoch.published(), baseline);
    }
}

#[test]
fn frozen_queue_info_rejects_every_admission_contract_change() {
    let baseline = test_queue_info(8);
    let changes = [
        changed_queue_info(baseline, |info| info.id = 1),
        changed_queue_info(baseline, |info| info.device.num_blocks += 1),
        changed_queue_info(baseline, |info| info.device.logical_block_size *= 2),
        changed_queue_info(baseline, |info| info.device.read_only = true),
        changed_queue_info(baseline, |info| info.device.name = Some("changed-name")),
        changed_queue_info(baseline, |info| {
            info.device.vendor = Some("changed-vendor");
        }),
        changed_queue_info(baseline, |info| {
            info.device.model = Some("changed-model");
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            info.limits.dma = dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Translated(core::num::NonZeroU64::new(1).unwrap()),
                dma.coherency(),
                dma.constraints(),
            );
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            info.limits.dma = dma_api::DmaDeviceInfo::new(
                dma.domain(),
                dma_api::DmaCoherency::Coherent,
                dma.constraints(),
            );
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            let mut constraints = dma.constraints();
            constraints.addr_mask -= 1;
            info.limits.dma =
                dma_api::DmaDeviceInfo::new(dma.domain(), dma.coherency(), constraints);
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            let mut constraints = dma.constraints();
            constraints.align *= 2;
            info.limits.dma =
                dma_api::DmaDeviceInfo::new(dma.domain(), dma.coherency(), constraints);
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            let mut constraints = dma.constraints();
            constraints.boundary = Some(4096);
            info.limits.dma =
                dma_api::DmaDeviceInfo::new(dma.domain(), dma.coherency(), constraints);
        }),
        changed_queue_info(baseline, |info| {
            let dma = info.limits.dma;
            let mut constraints = dma.constraints();
            constraints.max_segment_size = Some(4096);
            info.limits.dma =
                dma_api::DmaDeviceInfo::new(dma.domain(), dma.coherency(), constraints);
        }),
        changed_queue_info(baseline, |info| info.limits.dma_length_alignment *= 2),
        changed_queue_info(baseline, |info| info.limits.max_inflight -= 1),
        changed_queue_info(baseline, |info| info.limits.max_submit_batch -= 1),
        changed_queue_info(baseline, |info| {
            info.limits.max_blocks_per_request += 1;
        }),
        changed_queue_info(baseline, |info| info.limits.max_segments += 1),
        changed_queue_info(baseline, |info| {
            info.limits.supported_flags = RequestFlags::FUA;
        }),
        changed_queue_info(baseline, |info| info.limits.supports_flush = false),
    ];

    for observed in changes {
        let mut epoch = QueueInfoEpoch::new(baseline);
        epoch.freeze();
        assert_eq!(epoch.observe(observed), Err(BlkError::InvalidRequest));
        assert_eq!(epoch.published(), baseline);
    }

    let mut epoch = QueueInfoEpoch::new(baseline);
    epoch.freeze();
    assert_eq!(epoch.observe(baseline), Ok(()));
}
