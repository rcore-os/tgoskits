use super::*;

impl RequestSlot {
    pub(in crate::block) fn pending_for_test(id: RequestId) -> Self {
        Self {
            state: SlotState::Pending,
            active_identity: CommandIdentity::new(1, 1),
            cid_generation: 1,
            accepted: Some(AcceptedRequest {
                id,
                request: OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
                dma: None,
            }),
            prp_list: None,
        }
    }

    pub(in crate::block) fn runtime_id(&self) -> Option<RequestId> {
        self.accepted.as_ref().map(|request| request.id)
    }
}

impl NvmeQueueState {
    fn cid_epoch_for_test(&self) -> u64 {
        self.cid_epoch
    }
}

use alloc::vec::Vec;

use rdif_block::{CompletedRequest, CompletionSink};

use crate::block::{CachedCompletion, CompletionStatus};

#[derive(Default)]
struct CountingSink {
    completions: Vec<CompletedRequest>,
}

impl CompletionSink for CountingSink {
    fn complete(&mut self, completion: CompletedRequest) {
        self.completions.push(completion);
    }
}

#[test]
fn irq_completion_then_recovery_reclaim_emits_one_terminal_result() {
    let runtime_id = RequestId::new(0x55aa);
    let mut state = NvmeQueueState::new(1, Vec::new());
    let identity = state.alloc_identity().expect("one CID must be available");
    state.accept(identity, accepted_flush(runtime_id), None);
    let mut cache = CompletionCache::new(2);
    assert!(cache.record(CachedCompletion {
        identity,
        status: CompletionStatus {
            success: true,
            raw_status: 0,
            result: 0,
        },
    }));
    let mut sink = CountingSink::default();

    assert_eq!(
        state
            .emit_cached_completions(0, &mut cache, 64, &mut sink)
            .expect("valid CQE must complete its accepted request"),
        1
    );
    state.cancel_all(&mut sink);

    assert_eq!(sink.completions.len(), 1);
    assert_eq!(sink.completions[0].id, runtime_id);
    assert_eq!(sink.completions[0].result, Ok(()));
    assert!(!state.has_accepted());
}

#[test]
fn repeated_quiesced_reclaim_cancels_each_accepted_request_once() {
    let runtime_id = RequestId::new(7);
    let mut state = NvmeQueueState::new(1, Vec::new());
    let identity = state.alloc_identity().expect("one CID must be available");
    state.accept(identity, accepted_flush(runtime_id), None);
    let mut sink = CountingSink::default();

    state.cancel_all(&mut sink);
    state.cancel_all(&mut sink);

    assert_eq!(sink.completions.len(), 1);
    assert_eq!(sink.completions[0].id, runtime_id);
    assert_eq!(sink.completions[0].result, Err(BlkError::Cancelled));
    assert!(!state.has_accepted());
}

#[test]
fn invalid_cached_cid_prevents_all_terminal_publication_in_the_batch() {
    let runtime_id = RequestId::new(9);
    let mut state = NvmeQueueState::new(2, Vec::new());
    let identity = state.alloc_identity().expect("one CID must be available");
    assert_eq!(identity.slot(), 1);
    state.accept(identity, accepted_flush(runtime_id), None);
    let mut cache = CompletionCache::new(3);
    assert!(cache.record(CachedCompletion {
        identity,
        status: CompletionStatus {
            success: true,
            raw_status: 0,
            result: 0,
        },
    }));
    assert!(cache.record(CachedCompletion {
        identity: CommandIdentity::new(2, 1).unwrap(),
        status: CompletionStatus {
            success: false,
            raw_status: 0xdead,
            result: 0,
        },
    }));
    let mut sink = CountingSink::default();

    assert_eq!(
        state.emit_cached_completions(0, &mut cache, 64, &mut sink),
        Err(BlkError::Io)
    );
    assert!(
        sink.completions.is_empty(),
        "queue corruption must be diagnosed before any completion callback"
    );
    assert!(state.has_accepted());
}

#[test]
fn late_cqe_cannot_complete_a_reused_hardware_slot() {
    let first_id = RequestId::new(0x31);
    let second_id = RequestId::new(0x32);
    let mut state = NvmeQueueState::new(1, Vec::new());
    let identity = state.alloc_identity().expect("one CID must be available");
    state.accept(identity, accepted_flush(first_id), None);
    let late_completion = CachedCompletion {
        identity,
        status: CompletionStatus {
            success: true,
            raw_status: 0,
            result: 0,
        },
    };
    let mut first_cache = CompletionCache::new(2);
    assert!(first_cache.record(late_completion));
    let mut sink = CountingSink::default();
    assert_eq!(
        state
            .emit_cached_completions(0, &mut first_cache, 1, &mut sink)
            .expect("the first request must complete"),
        1
    );

    let reused_identity = state.alloc_identity().expect("the slot must be reusable");
    assert_eq!(reused_identity.slot(), identity.slot());
    assert_ne!(reused_identity, identity);
    state.accept(reused_identity, accepted_flush(second_id), None);
    let mut late_cache = CompletionCache::new(2);
    assert!(late_cache.record(late_completion));

    assert_eq!(
        state.emit_cached_completions(0, &mut late_cache, 1, &mut sink),
        Err(BlkError::Io),
        "a CQE from the previous slot generation must be rejected"
    );
    assert_eq!(sink.completions.len(), 1);
    assert_eq!(sink.completions[0].id, first_id);
    assert!(state.has_accepted());
}

#[test]
fn cid_fragment_exhaustion_requires_a_quiesced_queue_epoch() {
    let mut state = NvmeQueueState::new(1, Vec::new());
    let initial_epoch = state.cid_epoch_for_test();

    for expected_generation in 1..=CommandIdentity::MAX_GENERATION {
        let identity = state
            .alloc_identity()
            .expect("the generation fragment must not wrap early");
        assert_eq!(identity.slot(), 1);
        assert_eq!(identity.generation(), expected_generation);
        state.release_unaccepted(identity);
    }
    assert_eq!(
        state.alloc_identity(),
        Err(CidAllocationError::GenerationExhausted)
    );
    assert_eq!(
        CidAllocationError::GenerationExhausted.into_block_error(),
        BlkError::QueueEpochExhausted
    );

    assert!(state.advance_cid_epoch_after_quiesce());
    assert_eq!(state.cid_epoch_for_test(), initial_epoch + 1);
    let identity = state
        .alloc_identity()
        .expect("a proved queue epoch transition may reuse generation one");
    assert_eq!(identity.generation(), 1);
}

fn accepted_flush(id: RequestId) -> AcceptedRequest {
    AcceptedRequest {
        id,
        request: OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        },
        dma: None,
    }
}
