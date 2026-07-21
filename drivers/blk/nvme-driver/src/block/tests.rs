extern crate std;

use core::{
    alloc::Layout,
    num::{NonZeroU64, NonZeroUsize},
    ptr::NonNull,
};
use std::alloc::{alloc_zeroed, dealloc};

use dma_api::{
    CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle,
    DmaOp,
};
use rdif_block::{
    DriverEvidenceRetirement, IrqSourceId, OwnedRequest, QueueExecution, QueueKind, RequestFlags,
    RequestId, RequestOp,
};

use super::{
    AcceptedRequest, CachedCompletion, CommandIdentity, CompletionCache, CompletionStatus,
    NVME_QUEUE_EXECUTION, NvmeEvidenceDisposition, NvmeEvidenceFacts, NvmeEvidenceLedger,
    PrpPageAccumulator, RequestSlot, SlotState, controller::effective_queue_depth,
    drain_completion_source, evidence_ledger::NvmeEvidenceError, irq_sources_from_queue_bits,
    limits, prepare_request_dma, queue_interrupt_sources, source_queue_bits,
};
use crate::{Namespace, queue::NvmeCompletion};

struct TestDma;

impl DmaOp for TestDma {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe { DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { self.dealloc_contiguous(handle) };
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        let layout = Layout::from_size_align(size.get(), 1)?;
        Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

static TEST_DMA: TestDma = TestDma;

fn dma_buffer(direction: DmaDirection) -> CpuDmaBuffer {
    CpuDmaBuffer::new_zero(
        &DeviceDma::new_legacy(u64::MAX, &TEST_DMA),
        NonZeroUsize::new(512).expect("test DMA length must be non-zero"),
        512,
        direction,
    )
    .expect("test DMA allocation must succeed")
}

#[test]
fn queue_limits_align_dma_to_nvme_page_size() {
    let namespace = Namespace {
        id: 1,
        lba_size: 512,
        lba_count: 1024,
        metadata_size: 0,
    };
    let limits = limits(u64::MAX, 4096, None, namespace, 8);

    assert_eq!(limits.dma_alignment, 4096);
    assert_eq!(limits.max_segments, 1);
    assert_eq!(limits.max_segment_size, 4096 * 513);
    assert!(limits.max_blocks_per_request >= 8);
    assert!(!limits.supports_flush);
}

#[test]
fn queue_limits_keep_prp_capacity_tied_to_controller_page() {
    let namespace = Namespace {
        id: 1,
        lba_size: 8192,
        lba_count: 1024,
        metadata_size: 0,
    };
    let limits = limits(u64::MAX, 4096, None, namespace, 8);

    assert_eq!(limits.dma_alignment, 8192);
    assert_eq!(limits.max_segments, 1);
    assert_eq!(limits.max_segment_size, 8192 * 256);
    assert_eq!(limits.max_blocks_per_request, 256);
}

#[test]
fn queue_limits_respect_controller_transfer_limit() {
    let namespace = Namespace {
        id: 1,
        lba_size: 512,
        lba_count: 1024,
        metadata_size: 0,
    };
    let limits = limits(u64::MAX, 4096, Some(512 * 1024), namespace, 8);

    assert_eq!(limits.max_blocks_per_request, 1024);
    assert_eq!(limits.max_segment_size, 512 * 1024);
}

#[test]
fn effective_queue_depth_reserves_one_hardware_ring_entry() {
    assert_eq!(
        effective_queue_depth(64, 16).map(NonZeroUsize::get),
        Some(15)
    );
    assert_eq!(effective_queue_depth(8, 64).map(NonZeroUsize::get), Some(8));
    assert_eq!(effective_queue_depth(0, 2).map(NonZeroUsize::get), Some(1));
    assert_eq!(effective_queue_depth(64, 1), None);
}

#[test]
fn legacy_irq_source_covers_all_created_queues() {
    let sources = irq_sources_from_queue_bits(false, &[], 0b1011);

    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id, 0);
    assert_eq!(sources[0].queues.bits(), 0b1011);
    assert_eq!(source_queue_bits(false, &[], 0, 0b1011), 0b1011);
    assert_eq!(source_queue_bits(false, &[], 1, 0b1011), 0);
}

#[test]
fn msix_irq_sources_group_queues_by_vector() {
    let vectors = [4, 5, 4];
    let sources = irq_sources_from_queue_bits(true, &vectors, 0b111);

    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].id, 4);
    assert_eq!(sources[0].queues.bits(), 0b101);
    assert_eq!(sources[1].id, 5);
    assert_eq!(sources[1].queues.bits(), 0b010);
    assert_eq!(source_queue_bits(true, &vectors, 4, 0b111), 0b101);
}

#[test]
fn queue_interrupt_mask_matches_declared_logical_source() {
    let intx = queue_interrupt_sources(false, &[], 2);
    assert_eq!(intx.bits(), 1);

    let msix = queue_interrupt_sources(true, &[4, 5, 4], 1);
    assert_eq!(msix.bits(), 1 << 5);

    let kind = QueueKind::Interrupt { sources: msix };
    assert_eq!(kind, QueueKind::Interrupt { sources: msix });
    assert_eq!(NVME_QUEUE_EXECUTION, QueueExecution::Tagged);
}

#[test]
fn completion_source_stops_at_budget_and_retains_the_captured_batch() {
    let mut cache = CompletionCache::new(4);
    let mut completions = [
        Some(CachedCompletion::success(1)),
        Some(CachedCompletion::success(2)),
        Some(CachedCompletion::success(3)),
    ]
    .into_iter();

    let progress = drain_completion_source(|| completions.next().flatten(), &mut cache, 2);

    assert_eq!(progress.completed, 2);
    assert!(progress.may_have_more);
}

#[test]
fn request_slot_preserves_runtime_id_independently_of_hardware_cid() {
    let runtime_id = RequestId::new(0x5a5a);
    let slot = RequestSlot::pending_for_test(runtime_id);

    assert_eq!(slot.runtime_id(), Some(runtime_id));
    assert_ne!(
        usize::from(runtime_id),
        1,
        "hardware CID is not the runtime ID"
    );
}

#[test]
fn accepted_completion_returns_exact_runtime_id_and_dma_buffer() {
    let runtime_id = RequestId::new(0xabc);
    let buffer = dma_buffer(DmaDirection::FromDevice);
    let original_ptr = buffer.cpu_ptr();
    let request = OwnedRequest {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: Some(buffer),
        flags: RequestFlags::NONE,
    };
    let (request, prepared) =
        prepare_request_dma(runtime_id, request).expect("matching read DMA must be accepted");
    let prepared = prepared.expect("read requests must prepare DMA");
    assert!(request.data.is_none());
    assert_eq!(prepared.cpu_ptr(), original_ptr);

    // SAFETY: this test models an accepted command on a controller that
    // cannot actually access the test allocation.
    let dma = unsafe { prepared.into_in_flight() };
    let accepted = AcceptedRequest {
        id: runtime_id,
        request,
        dma: Some(dma),
    };
    // SAFETY: no hardware exists in this unit test, so the modelled command
    // is already quiesced and cannot retain bus-master ownership.
    let completed = unsafe { accepted.complete_after_quiesce(Ok(())) };

    assert_eq!(completed.id, runtime_id);
    assert_eq!(completed.result, Ok(()));
    assert_eq!(
        completed
            .request
            .data
            .as_ref()
            .expect("completion must return the full request buffer")
            .cpu_ptr(),
        original_ptr
    );
}

#[test]
fn bidirectional_dma_is_accepted_for_read_and_write_requests() {
    for op in [RequestOp::Read, RequestOp::Write] {
        let request = OwnedRequest {
            op,
            lba: 0,
            block_count: 1,
            data: Some(dma_buffer(DmaDirection::Bidirectional)),
            flags: RequestFlags::NONE,
        };

        let (_, prepared) = prepare_request_dma(RequestId::new(0x71), request)
            .expect("RDIF-valid bidirectional DMA must remain usable by NVMe");
        assert!(prepared.is_some());
    }
}

#[test]
fn prp_pages_split_at_controller_page_boundaries() {
    let mut pages = PrpPageAccumulator::new();

    pages.push_segment(0x1800, 4096, 4096).unwrap();

    assert_eq!(pages.into_pages(), [0x1800, 0x2000]);
}

#[test]
fn prp_pages_coalesce_contiguous_split_segments() {
    let mut pages = PrpPageAccumulator::new();

    pages.push_segment(0x1000, 4096, 4096).unwrap();
    pages.push_segment(0x2000, 2048, 4096).unwrap();
    pages.push_segment(0x2800, 2048, 4096).unwrap();

    assert_eq!(pages.into_pages(), [0x1000, 0x2000]);
}

#[test]
fn prp_pages_reject_unaligned_non_contiguous_segment() {
    let mut pages = PrpPageAccumulator::new();

    pages.push_segment(0x1000, 2048, 4096).unwrap();

    assert!(pages.push_segment(0x2800, 512, 4096).is_err());
}

#[test]
fn cached_completion_does_not_complete_slot_until_task_consumes_it() {
    let mut cache = CompletionCache::new(4);
    let slot = RequestSlot::pending_for_test(RequestId::new(9));

    assert!(cache.record(CachedCompletion::success(2)));

    assert_eq!(slot.state, SlotState::Pending);
    assert!(cache.has_ready());
    assert!(cache.take(2).is_some());
}

#[test]
fn cached_failed_completion_preserves_error_for_task_context() {
    let mut cache = CompletionCache::new(4);

    assert!(cache.record(CachedCompletion::failed(3, 0x4002)));
    let status = cache
        .take(3)
        .expect("cached completion must be present")
        .status;

    assert!(!status.success);
    assert_eq!(status.raw_status, 0x4002);
}

#[test]
fn cached_completion_is_consumed_once() {
    let mut cache = CompletionCache::new(2);

    assert!(cache.record(CachedCompletion::success(1)));

    assert!(cache.take(1).is_some());
    assert!(cache.take(1).is_none());
}

#[test]
fn completion_cache_rejects_reserved_and_duplicate_cids() {
    let mut cache = CompletionCache::new(2);

    assert!(
        CachedCompletion::from_nvme(NvmeCompletion {
            command_id: 0,
            ..NvmeCompletion::default()
        })
        .is_none()
    );
    assert!(cache.record(CachedCompletion::failed(1, 0x4002)));
    assert!(!cache.record(CachedCompletion::success(1)));
    assert_eq!(
        cache
            .take(1)
            .expect("duplicate CQE must not evict the original status")
            .status
            .raw_status,
        0x4002
    );
}

#[test]
fn quiesced_reset_discards_stale_completion_before_cid_reuse() {
    let mut cache = CompletionCache::new(2);
    assert!(cache.record(CachedCompletion::failed(1, 0x4002)));

    cache.clear_after_quiesce();

    assert!(cache.take(1).is_none());
    assert!(cache.record(CachedCompletion::success(1)));
    assert!(
        cache
            .take(1)
            .expect("fresh post-reset CQE must use the reused CID")
            .status
            .success
    );
}

#[test]
fn hard_irq_capture_never_consumes_admin_or_io_completion_queues() {
    let irq = [include_str!("irq/mod.rs"), include_str!("irq/topology.rs")].concat();
    let controller = include_str!("controller.rs");
    let completion = include_str!("completion.rs");
    let queue_core = [
        include_str!("queue_runtime/core/mod.rs"),
        include_str!("queue_runtime/core/submission.rs"),
        include_str!("queue_runtime/core/completion_owner.rs"),
    ]
    .concat();

    for forbidden in [
        "drain_admin_irq_completion",
        "drain_irq_completions",
        "IrqCompletionBudget",
        "capture_queue_irq(",
        "NvmeBlockOwner",
    ] {
        assert!(
            !irq.contains(forbidden),
            "hard IRQ capture must not consume CQ state through {forbidden}"
        );
    }
    assert!(
        irq.contains("NvmeIrqState"),
        "the IRQ action must retain the narrow source-mask capability"
    );
    assert!(
        irq.contains("NvmeCompletionProbe"),
        "the IRQ action must classify shared lines through a read-only CQ phase probe"
    );
    assert!(
        !controller.contains("admin_cq_claimed"),
        "the fixed maintenance owner must be the only admin CQ consumer"
    );
    for forbidden in ["cq_claimed", "try_with_cq_claim"] {
        assert!(
            !queue_core.contains(forbidden),
            "the fixed maintenance owner must not contend for I/O CQ ownership through {forbidden}"
        );
    }
    for forbidden in ["AtomicBool", "AtomicU16", "AtomicU64"] {
        assert!(
            !completion.contains(forbidden),
            "the owner-local completion cache must not retain the old IRQ/task publication \
             primitive {forbidden}"
        );
    }
}

#[test]
fn v13_io_owner_does_not_reintroduce_the_legacy_shared_queue_core() {
    let v13 = [
        include_str!("v13/mod.rs"),
        include_str!("v13/control.rs"),
        include_str!("v13/topology.rs"),
    ]
    .concat();
    let domain = include_str!("io_domain.rs");
    let owned = include_str!("queue_runtime/owned.rs");
    let owner_path = [v13.as_str(), domain, owned].concat();

    for forbidden in [
        "NvmeQueueCore",
        "AtomicClaim",
        "try_claim_state",
        "Arc<HardwareQueue",
        "Arc<NvmeQueue",
        "UnsafeCell",
    ] {
        assert!(
            !owner_path.contains(forbidden),
            "v0.13 final queue ownership must not depend on {forbidden}"
        );
    }
    assert!(
        owned.contains("&mut self"),
        "the final queue owner must expose mutation through an exclusive borrow"
    );
}

#[test]
fn fixed_evidence_ledger_merges_one_shared_source_without_exposing_queue_facts() {
    let source = IrqSourceId::new(7).expect("test source must fit the fixed ledger");
    let ledger = NvmeEvidenceLedger::new(source, 3);
    let lifecycle = NonZeroU64::new(11).expect("test lifecycle must be nonzero");

    let first = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 2))
        .expect("the empty source ledger must accept evidence");
    let merged = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 5))
        .expect("duplicate shared-source evidence must coalesce in the driver ledger");

    assert_eq!(merged, first);
    assert_eq!(first.source(), source);
    assert_eq!(first.slot(), 3);
    let batch = ledger
        .begin_service(first)
        .expect("the exact published identity must own the ledger slot");
    assert_eq!(
        batch.facts(),
        NvmeEvidenceFacts::queues((1 << 2) | (1 << 5))
    );
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn shared_source_keeps_admin_fact_outside_the_full_queue_bitmap() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 4);
    let lifecycle = NonZeroU64::new(13).expect("test lifecycle must be nonzero");

    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::admin())
        .expect("the admin CQ fact must publish independently");
    assert_eq!(
        ledger
            .publish(lifecycle, NvmeEvidenceFacts::queues(u64::MAX))
            .expect("all 64 I/O queue facts must coalesce without stealing the admin bit"),
        evidence
    );

    let batch = ledger
        .begin_service(evidence)
        .expect("the combined evidence must remain linearly serviceable");
    assert!(batch.facts().has_admin());
    assert_eq!(batch.facts().queue_bits(), u64::MAX);
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn shared_evidence_moves_io_then_control_without_copying_its_identity() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 12);
    let lifecycle = NonZeroU64::new(17).expect("test lifecycle must be nonzero");
    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 4).with_admin())
        .expect("one shared IRQ must publish one aggregate evidence identity");

    let io_pass = ledger
        .begin_service(evidence)
        .expect("the I/O owner must claim the exact aggregate identity");
    assert_eq!(io_pass.facts().queue_bits(), 1 << 4);
    assert!(io_pass.facts().has_admin());
    assert_eq!(
        ledger.finish_service(io_pass, NvmeEvidenceFacts::admin()),
        NvmeEvidenceDisposition::Retained
    );

    let control_pass = ledger
        .begin_service(evidence)
        .expect("control must receive the same retained evidence identity");
    assert!(control_pass.facts().has_admin());
    assert_eq!(control_pass.facts().queue_bits(), 0);
    assert_eq!(
        ledger.finish_service(control_pass, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn irq_merge_cannot_publish_a_second_owner_while_service_batch_is_live() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 13);
    let lifecycle = NonZeroU64::new(19).expect("test lifecycle must be nonzero");
    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 2))
        .expect("the first IRQ fact must publish");
    let first_owner = ledger
        .begin_service(evidence)
        .expect("the first service pass must own the evidence");

    assert_eq!(
        ledger
            .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 5))
            .expect("a racing IRQ must merge into the live identity"),
        evidence
    );
    assert!(
        matches!(
            ledger.begin_service(evidence),
            Err(NvmeEvidenceError::PublicationInProgress)
        ),
        "the merged fact must not mint a second service owner"
    );
    assert_eq!(
        ledger.finish_service(first_owner, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Retained
    );

    let retained = ledger
        .begin_service(evidence)
        .expect("the first owner release must expose the merged fact");
    assert_eq!(retained.facts(), NvmeEvidenceFacts::queues(1 << 5));
    assert_eq!(
        ledger.finish_service(retained, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn retained_evidence_keeps_the_same_linear_identity_until_fully_drained() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 9);
    let lifecycle = NonZeroU64::new(3).expect("test lifecycle must be nonzero");
    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1))
        .expect("the empty source ledger must accept evidence");
    let batch = ledger
        .begin_service(evidence)
        .expect("published evidence must be serviceable exactly once per pass");

    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::queues(1)),
        NvmeEvidenceDisposition::Retained
    );
    let retained = ledger
        .begin_service(evidence)
        .expect("retained hardware facts must keep their original identity");
    assert_eq!(retained.facts(), NvmeEvidenceFacts::queues(1));
    assert_eq!(
        ledger.finish_service(retained, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
    assert_eq!(
        ledger
            .commit_drained_evidence(evidence)
            .expect("runtime latch commit must retire the drained identity"),
        DriverEvidenceRetirement::Retired
    );
    let next = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 3))
        .expect("a drained slot may publish a new generation");
    assert_ne!(next.slot_generation(), evidence.slot_generation());
    assert!(
        ledger.begin_service(evidence).is_err(),
        "a stale evidence identity must not consume a later ledger epoch"
    );
    let next_batch = ledger
        .begin_service(next)
        .expect("the new slot generation must remain serviceable");
    assert_eq!(next_batch.facts(), NvmeEvidenceFacts::queues(1 << 3));
    assert_eq!(
        ledger.finish_service(next_batch, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

#[test]
fn drained_evidence_identity_is_not_reused_before_runtime_commit() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 10);
    let lifecycle = NonZeroU64::new(23).expect("test lifecycle must be nonzero");
    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1))
        .expect("the first IRQ fact must publish");
    let batch = ledger
        .begin_service(evidence)
        .expect("the published identity must begin service");
    assert_eq!(
        ledger.finish_service(batch, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );

    let raced = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 4))
        .expect("capture before runtime commit must remain publishable");

    assert_eq!(
        raced, evidence,
        "driver evidence cannot mint a new identity before runtime latch commit"
    );
    assert_eq!(
        ledger
            .commit_drained_evidence(evidence)
            .expect("a capture racing driver retirement must remain recoverable"),
        DriverEvidenceRetirement::Raced
    );
    let raced_batch = ledger
        .begin_service(evidence)
        .expect("the raced fact must remain under the old identity");
    assert_eq!(raced_batch.facts(), NvmeEvidenceFacts::queues(1 << 4));
    assert_eq!(
        ledger.finish_service(raced_batch, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
    assert_eq!(
        ledger
            .commit_drained_evidence(evidence)
            .expect("the clean second commit must retire the old identity"),
        DriverEvidenceRetirement::Retired
    );

    let next = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 5))
        .expect("a committed ledger may publish its next identity");
    assert_ne!(next.slot_generation(), evidence.slot_generation());
}

#[test]
fn dropping_an_unfinished_service_batch_retains_its_driver_evidence() {
    let source = IrqSourceId::new(0).expect("INTx source zero is valid");
    let ledger = NvmeEvidenceLedger::new(source, 1);
    let lifecycle = NonZeroU64::new(5).expect("test lifecycle must be nonzero");
    let evidence = ledger
        .publish(lifecycle, NvmeEvidenceFacts::queues(1 << 6))
        .expect("the empty source ledger must accept evidence");

    drop(
        ledger
            .begin_service(evidence)
            .expect("published evidence must begin one service pass"),
    );

    let retained = ledger
        .begin_service(evidence)
        .expect("Drop must return unfinished facts to the same ledger identity");
    assert_eq!(retained.facts(), NvmeEvidenceFacts::queues(1 << 6));
    assert_eq!(
        ledger.finish_service(retained, NvmeEvidenceFacts::default()),
        NvmeEvidenceDisposition::Drained
    );
}

impl CachedCompletion {
    const fn success(cid: usize) -> Self {
        Self {
            identity: CommandIdentity::new_for_test(cid, 1),
            status: CompletionStatus {
                success: true,
                raw_status: 0,
                result: 0,
            },
        }
    }

    const fn failed(cid: usize, raw_status: u16) -> Self {
        Self {
            identity: CommandIdentity::new_for_test(cid, 1),
            status: CompletionStatus {
                success: false,
                raw_status,
                result: 0,
            },
        }
    }
}
