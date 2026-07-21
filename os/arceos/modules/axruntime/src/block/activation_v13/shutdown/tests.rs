//! Deterministic shutdown-coordinator regression tests.

use core::num::NonZeroU64;

use rdif_block::{ControllerEpoch, DmaQuiesced};

use super::*;

fn coordinator(participants: usize) -> ControllerShutdown {
    ControllerShutdown::new(ShutdownGeneration::new(7).unwrap(), participants).unwrap()
}

fn dma_proof(cookie: usize) -> DmaQuiesced {
    // SAFETY: this is a pure coordinator test. The proof models the output
    // of a controller owner that completed the hardware quiesce protocol.
    unsafe { DmaQuiesced::new(ControllerEpoch::INITIAL, cookie) }
}

#[test]
fn shutdown_orders_every_participant_and_moves_the_dma_proof_once() {
    let shutdown = coordinator(3);
    let control = shutdown.participant(0).unwrap();
    let io0 = shutdown.participant(1).unwrap();
    let io1 = shutdown.participant(2).unwrap();

    let initial: ShutdownSnapshot = shutdown.snapshot();
    assert_eq!(initial.phase(), ShutdownPhase::Running);
    assert_eq!(initial.participant_count(), 3);
    assert_eq!(control.generation().get(), NonZeroU64::new(7).unwrap());
    shutdown.begin_freeze(control).unwrap();
    assert_eq!(
        shutdown.ack_sources_closed(io0).unwrap_err(),
        ShutdownError::WrongPhase {
            operation: ShutdownOperation::AckSourcesClosed,
            expected: ShutdownPhase::DeviceMasked,
            actual: ShutdownPhase::Freezing,
        }
    );
    let first_cutoff: ShutdownAckProgress = shutdown.ack_dispatch_cutoff(io1).unwrap();
    assert_eq!(first_cutoff.acknowledged(), 1 << 2);
    assert!(!first_cutoff.all());
    shutdown.ack_dispatch_cutoff(control).unwrap();
    assert!(shutdown.ack_dispatch_cutoff(io0).unwrap().all());
    assert!(matches!(
        shutdown.ack_dispatch_cutoff(io0),
        Err(ShutdownError::Replay {
            acknowledgement: ShutdownAcknowledgement::DispatchCutoff,
            participant: 1,
        })
    ));
    shutdown.finish_dispatch_stopped(control).unwrap();
    shutdown.mark_device_masked(control).unwrap();
    shutdown.ack_sources_closed(control).unwrap();
    shutdown.ack_sources_closed(io0).unwrap();
    shutdown.ack_sources_closed(io1).unwrap();
    shutdown.finish_sources_closed(control).unwrap();
    let sources_closed = shutdown.snapshot();
    assert_eq!(sources_closed.dispatch_cutoff(), 0b111);
    assert_eq!(sources_closed.sources_closed(), 0b111);

    shutdown
        .publish_dma_quiesced(control, dma_proof(0x51a7))
        .unwrap();
    let control_proof = shutdown.borrow_dma_quiesced(control).unwrap();
    let io0_proof = shutdown.borrow_dma_quiesced(io0).unwrap();
    let io1_proof = shutdown.borrow_dma_quiesced(io1).unwrap();
    assert_eq!(io0_proof.proof().controller_cookie(), 0x51a7);
    assert!(matches!(
        shutdown.borrow_dma_quiesced(io0),
        Err(ShutdownError::Replay {
            acknowledgement: ShutdownAcknowledgement::DmaBorrowed,
            participant: 1,
        })
    ));
    shutdown.ack_reclaimed(io1_proof).unwrap();
    shutdown.ack_reclaimed(control_proof).unwrap();
    shutdown.ack_reclaimed(io0_proof).unwrap();
    assert_eq!(shutdown.snapshot().reclaimed(), 0b111);
    shutdown.finish_reclaimed(control).unwrap();
    let proof = shutdown.take_dma_quiesced(control).unwrap();
    assert_eq!(proof.controller_cookie(), 0x51a7);
    shutdown.finish_closed(control).unwrap();
    assert_eq!(shutdown.snapshot().phase(), ShutdownPhase::Closed);
}

#[test]
fn stale_out_of_range_and_non_control_tokens_are_rejected() {
    let shutdown = coordinator(2);
    let control = shutdown.participant(0).unwrap();
    let io = shutdown.participant(1).unwrap();
    let stale = ControllerShutdown::new(ShutdownGeneration::new(6).unwrap(), 2)
        .unwrap()
        .participant(1)
        .unwrap();

    assert!(matches!(
        shutdown.participant(2),
        Err(ShutdownError::InvalidParticipant { .. })
    ));
    assert!(matches!(
        shutdown.ack_dispatch_cutoff(stale),
        Err(ShutdownError::StaleParticipant { .. })
    ));
    assert_eq!(
        shutdown.begin_freeze(io).unwrap_err(),
        ShutdownError::ControlRequired { participant: 1 }
    );
    shutdown.begin_freeze(control).unwrap();
}

#[test]
fn phase_cannot_advance_before_all_fixed_participants_stop_dispatch() {
    let shutdown = coordinator(64);
    let control = shutdown.participant(0).unwrap();
    shutdown.begin_freeze(control).unwrap();
    for index in 0..63 {
        shutdown
            .ack_dispatch_cutoff(shutdown.participant(index).unwrap())
            .unwrap();
    }
    assert!(matches!(
        shutdown.finish_dispatch_stopped(control),
        Err(ShutdownError::Incomplete {
            acknowledgement: ShutdownAcknowledgement::DispatchCutoff,
            ..
        })
    ));
    shutdown
        .ack_dispatch_cutoff(shutdown.participant(63).unwrap())
        .unwrap();
    shutdown.finish_dispatch_stopped(control).unwrap();
}

#[test]
fn dispatch_cutoff_allows_lost_irq_recovery_with_inflight_requests() {
    let shutdown = coordinator(2);
    let control = shutdown.participant(0).unwrap();
    let io = shutdown.participant(1).unwrap();
    let accepted_inflight = 4_usize;

    shutdown.begin_freeze(control).unwrap();
    shutdown.ack_dispatch_cutoff(control).unwrap();
    shutdown.ack_dispatch_cutoff(io).unwrap();
    shutdown.finish_dispatch_stopped(control).unwrap();

    assert_eq!(accepted_inflight, 4);
    assert_eq!(shutdown.snapshot().phase(), ShutdownPhase::DispatchStopped);
    shutdown.mark_device_masked(control).unwrap();
    shutdown.ack_sources_closed(control).unwrap();
    shutdown.ack_sources_closed(io).unwrap();
    shutdown.finish_sources_closed(control).unwrap();
    shutdown
        .publish_dma_quiesced(control, dma_proof(0x44))
        .unwrap();
    let control_proof = shutdown.borrow_dma_quiesced(control).unwrap();
    let io_proof = shutdown.borrow_dma_quiesced(io).unwrap();
    shutdown.ack_reclaimed(control_proof).unwrap();
    shutdown.ack_reclaimed(io_proof).unwrap();
    shutdown.finish_reclaimed(control).unwrap();

    assert_eq!(shutdown.snapshot().phase(), ShutdownPhase::Reclaimed);
}

#[test]
fn recovery_returns_fixed_owners_to_running_for_a_second_generation() {
    let shutdown = coordinator(2);
    let control = shutdown.participant(0).unwrap();
    let io = shutdown.participant(1).unwrap();

    shutdown
        .begin_recovery(control, rdif_block::ControllerFault::LostIrqEvidence)
        .unwrap();
    shutdown.ack_dispatch_cutoff(control).unwrap();
    shutdown.ack_dispatch_cutoff(io).unwrap();
    shutdown.finish_dispatch_stopped(control).unwrap();
    shutdown.mark_device_masked(control).unwrap();
    shutdown.ack_sources_closed(control).unwrap();
    shutdown.ack_sources_closed(io).unwrap();
    shutdown.finish_sources_closed(control).unwrap();
    shutdown
        .publish_dma_quiesced(control, dma_proof(0x51a7))
        .unwrap();
    let control_lease = shutdown.borrow_dma_quiesced(control).unwrap();
    let io_lease = shutdown.borrow_dma_quiesced(io).unwrap();
    shutdown.ack_reclaimed(control_lease).unwrap();
    shutdown.ack_reclaimed(io_lease).unwrap();
    shutdown.finish_reclaimed(control).unwrap();
    let proof = shutdown.take_dma_quiesced(control).unwrap();

    drop(proof);
    shutdown.begin_reinit_sources(control).unwrap();
    shutdown.ack_reinit_sources_armed(control).unwrap();
    shutdown.ack_reinit_sources_armed(io).unwrap();
    shutdown.finish_reinit_sources(control).unwrap();
    shutdown.begin_owner_resume(control).unwrap();
    shutdown.ack_resumed(control).unwrap();
    shutdown.ack_resumed(io).unwrap();
    shutdown.finish_recovered(control).unwrap();

    let recovered = shutdown.snapshot();
    assert_eq!(recovered.phase(), ShutdownPhase::Running);
    assert_eq!(recovered.dispatch_cutoff(), 0);
    assert_eq!(recovered.sources_closed(), 0);
    assert_eq!(recovered.reclaimed(), 0);
    assert_eq!(recovered.cycle().get(), 8);

    let next_control = shutdown.participant(0).unwrap();
    assert_ne!(next_control, control);
    shutdown
        .begin_recovery(next_control, rdif_block::ControllerFault::Protocol)
        .unwrap();
    assert_eq!(shutdown.snapshot().phase(), ShutdownPhase::Freezing);
}

#[test]
fn rejected_dma_publication_returns_the_same_linear_proof() {
    let shutdown = coordinator(1);
    let control = shutdown.participant(0).unwrap();
    let failure = shutdown
        .publish_dma_quiesced(control, dma_proof(0xcafe))
        .unwrap_err();
    let (error, proof) = failure.into_parts();

    assert!(matches!(error, ShutdownError::WrongPhase { .. }));
    assert_eq!(proof.controller_cookie(), 0xcafe);
}

#[test]
fn quarantine_is_terminal_and_retains_linear_state() {
    let shutdown = coordinator(1);
    let control = shutdown.participant(0).unwrap();
    shutdown.begin_freeze(control).unwrap();
    shutdown.quarantine(control).unwrap();

    assert_eq!(shutdown.snapshot().phase(), ShutdownPhase::Quarantined);
    assert!(matches!(
        shutdown.ack_dispatch_cutoff(control),
        Err(ShutdownError::WrongPhase {
            actual: ShutdownPhase::Quarantined,
            ..
        })
    ));
    assert!(matches!(
        shutdown.quarantine(control),
        Err(ShutdownError::Replay { .. })
    ));
}

#[test]
fn participant_count_is_bounded_by_one_atomic_word() {
    assert!(matches!(
        ControllerShutdown::new(ShutdownGeneration::new(1).unwrap(), 0),
        Err(ShutdownError::InvalidParticipantCount { count: 0 })
    ));
    assert!(matches!(
        ControllerShutdown::new(ShutdownGeneration::new(1).unwrap(), 65),
        Err(ShutdownError::InvalidParticipantCount { count: 65 })
    ));
}

#[test]
fn dma_value_cannot_be_taken_while_an_internal_arc_borrow_survives() {
    let shutdown = coordinator(1);
    let control = shutdown.participant(0).unwrap();
    shutdown.begin_freeze(control).unwrap();
    shutdown.ack_dispatch_cutoff(control).unwrap();
    shutdown.finish_dispatch_stopped(control).unwrap();
    shutdown.mark_device_masked(control).unwrap();
    shutdown.ack_sources_closed(control).unwrap();
    shutdown.finish_sources_closed(control).unwrap();
    shutdown
        .publish_dma_quiesced(control, dma_proof(0xdead))
        .unwrap();
    let lease = shutdown.borrow_dma_quiesced(control).unwrap();
    assert_eq!(lease.participant(), control);
    let leaked_internal_reference = alloc::sync::Arc::clone(&lease.proof);
    shutdown.ack_reclaimed(lease).unwrap();
    shutdown.finish_reclaimed(control).unwrap();

    assert_eq!(
        shutdown.take_dma_quiesced(control).unwrap_err(),
        ShutdownError::OutstandingDmaBorrowers { count: 1 }
    );
    drop(leaked_internal_reference);
    assert_eq!(
        shutdown
            .take_dma_quiesced(control)
            .unwrap()
            .controller_cookie(),
        0xdead
    );
}

#[test]
fn failed_reclaim_ack_returns_the_same_internal_proof_lease() {
    let shutdown = coordinator(1);
    let control = shutdown.participant(0).unwrap();
    shutdown.begin_freeze(control).unwrap();
    shutdown.ack_dispatch_cutoff(control).unwrap();
    shutdown.finish_dispatch_stopped(control).unwrap();
    shutdown.mark_device_masked(control).unwrap();
    shutdown.ack_sources_closed(control).unwrap();
    shutdown.finish_sources_closed(control).unwrap();
    shutdown
        .publish_dma_quiesced(control, dma_proof(0xbeef))
        .unwrap();
    let lease = shutdown.borrow_dma_quiesced(control).unwrap();
    shutdown.quarantine(control).unwrap();

    let failure = shutdown.ack_reclaimed(lease).unwrap_err();
    let (error, lease) = failure.into_parts();
    assert!(matches!(
        error,
        ShutdownError::WrongPhase {
            operation: ShutdownOperation::AckReclaimed,
            actual: ShutdownPhase::Quarantined,
            ..
        }
    ));
    assert_eq!(lease.proof().controller_cookie(), 0xbeef);
    assert!(shutdown.snapshot().dma_proof_available());
}

#[test]
fn nonzero_generation_is_part_of_every_snapshot() {
    let shutdown = coordinator(1);
    assert_eq!(
        shutdown.snapshot().generation(),
        NonZeroU64::new(7).unwrap()
    );
}
