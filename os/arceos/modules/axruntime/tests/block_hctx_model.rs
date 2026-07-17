#[path = "../src/block/hctx_model.rs"]
mod hctx_model;

use hctx_model::{
    DispatchArbiter, DispatchSource, HctxAccessGate, HctxCause, HctxControl, HctxPhase,
    HctxTerminalGate, ServiceBudget, ServiceContinuation, ServiceStage,
};

#[test]
fn service_order_is_error_timeout_completion_then_dispatch() {
    let control = HctxControl::new();
    assert!(!control.has_irq_or_error_pending());
    control.raise(HctxCause::Submit);
    control.raise(HctxCause::Timeout);
    control.raise(HctxCause::Irq);
    assert!(control.has_irq_or_error_pending());

    let batch = control.take_service_batch();
    assert_eq!(
        batch.stages().collect::<Vec<_>>(),
        vec![
            ServiceStage::IrqAndError,
            ServiceStage::TimeoutAndCancel,
            ServiceStage::CompletionAndWake,
            ServiceStage::Dispatch,
        ]
    );
}

#[test]
fn a_cause_raised_after_take_remains_pending_for_rerun() {
    let control = HctxControl::new();
    control.raise(HctxCause::Irq);

    let first = control.take_service_batch();
    control.raise(HctxCause::Submit);

    assert!(first.contains(HctxCause::Irq));
    assert!(!first.contains(HctxCause::Submit));
    assert!(control.has_pending());
    assert!(control.take_service_batch().contains(HctxCause::Submit));
}

#[test]
fn irq_publication_and_timeout_cutoff_share_one_linearization_gate() {
    let gate = HctxTerminalGate::new();
    let irq = gate
        .begin_irq_publication()
        .expect("the first IRQ publication enters the open gate");

    assert!(
        gate.try_begin_terminal().is_none(),
        "timeout cannot pass an IRQ publisher that entered first"
    );
    drop(irq);

    let terminal = gate
        .try_begin_terminal()
        .expect("the drained IRQ gate admits one terminal cutoff");
    assert!(
        gate.begin_irq_publication().is_none(),
        "an IRQ that starts after the cutoff is ordered after timeout"
    );
    drop(terminal);
    assert!(gate.begin_irq_publication().is_some());
}

#[test]
fn one_hctx_callback_has_one_budget_for_every_service_stage() {
    let mut budget = ServiceBudget::new(64).unwrap();

    assert_eq!(budget.consume(63), Ok(()));
    assert_eq!(budget.remaining(), 1);
    assert!(matches!(
        budget.consume(2),
        Err(hctx_model::ServiceBudgetError::Exhausted {
            requested: 2,
            remaining: 1,
        })
    ));
    assert_eq!(budget.remaining(), 1);
    assert_eq!(budget.consume(1), Ok(()));
    assert!(budget.is_exhausted());
}

#[test]
fn service_budget_consumption_is_an_unconditional_state_transition() {
    let mut budget = ServiceBudget::new(4).unwrap();

    let consumed = budget.consume(3);

    assert_eq!(consumed, Ok(()));
    assert_eq!(budget.remaining(), 1);
}

#[test]
fn recovery_changes_epoch_and_rejects_late_irq_events() {
    let control = HctxControl::new();
    let old_epoch = control.epoch();

    let recovering = control.begin_recovery().unwrap();
    assert_eq!(recovering.phase(), HctxPhase::Recovering);
    assert_ne!(recovering.epoch(), old_epoch);
    assert!(!control.accepts_event(old_epoch));
    assert!(!control.accepts_event(recovering.epoch()));

    control.begin_reinitialization(recovering).unwrap();
    control.finish_reinitialization().unwrap();
    assert_eq!(control.phase(), HctxPhase::Running);
}

#[test]
fn accepted_irq_epoch_cannot_be_relabelled_by_a_racing_recovery() {
    let control = HctxControl::new();
    let accepted_epoch = control
        .accepted_event_epoch()
        .expect("a running queue accepts IRQ evidence");

    let recovering = control.begin_recovery().unwrap();
    control.begin_reinitialization(recovering).unwrap();
    control.finish_reinitialization().unwrap();

    assert!(!control.accepts_event(accepted_epoch));
    assert_ne!(control.accepted_event_epoch(), Some(accepted_epoch));
}

#[test]
fn controller_can_recover_the_exact_hctx_transition_after_async_drain() {
    let control = HctxControl::new();
    let started = control.begin_recovery().unwrap();

    let drained = control.recovery_transition().unwrap();

    assert_eq!(drained, started);
    control.begin_reinitialization(drained).unwrap();
    control.finish_reinitialization().unwrap();
}

#[test]
fn hctx_dispatch_list_precedes_round_robin_software_contexts() {
    let mut arbiter = DispatchArbiter::<4>::new();
    let ready = [true, false, true, true];

    assert_eq!(
        arbiter.select(true, &ready),
        Some(DispatchSource::HardwareDispatchList)
    );
    assert_eq!(arbiter.select(false, &ready), Some(DispatchSource::Cpu(0)));
    assert_eq!(arbiter.select(false, &ready), Some(DispatchSource::Cpu(2)));
    assert_eq!(arbiter.select(false, &ready), Some(DispatchSource::Cpu(3)));
    assert_eq!(arbiter.select(false, &ready), Some(DispatchSource::Cpu(0)));
}

#[test]
fn lifecycle_disallows_guest_ownership_without_quiescing() {
    let control = HctxControl::new();
    assert!(control.enter_guest_owned().is_err());

    let quiescing = control.begin_quiesce().unwrap();
    assert_eq!(quiescing.phase(), HctxPhase::Quiescing);
    control.finish_detach(quiescing).unwrap();
    assert_eq!(control.phase(), HctxPhase::Detached);
    control.enter_guest_owned().unwrap();
    assert_eq!(control.phase(), HctxPhase::GuestOwned);

    let returning = control.begin_guest_return_recovery().unwrap();
    assert_eq!(returning.phase(), HctxPhase::Recovering);
    control.begin_reinitialization(returning).unwrap();
    assert_eq!(control.phase(), HctxPhase::Reinitializing);
    control.mark_offline().unwrap();
    assert_eq!(control.phase(), HctxPhase::Offline);
}

#[test]
fn non_destructive_quiesce_can_be_canceled_with_the_exact_permit() {
    let control = HctxControl::new();
    let quiescing = control.begin_quiesce().unwrap();

    control.cancel_quiesce(quiescing).unwrap();

    assert_eq!(control.phase(), HctxPhase::Running);
    assert!(control.accepts_submission());
    assert!(control.finish_detach(quiescing).is_err());
}

#[test]
fn quiescing_stops_admission_but_keeps_servicing_accepted_work() {
    let control = HctxControl::new();
    let live_epoch = control.epoch();

    let quiescing = control.begin_quiesce().unwrap();

    assert_eq!(quiescing.phase(), HctxPhase::Quiescing);
    assert!(!control.accepts_submission());
    assert!(control.services_accepted_work());
    assert!(control.accepts_event(live_epoch));
}

#[test]
fn timeout_while_quiescing_enters_a_new_recovery_epoch() {
    let control = HctxControl::new();
    let draining_epoch = control.begin_quiesce().unwrap().epoch();

    let recovering = control.begin_recovery().unwrap();

    assert_eq!(recovering.phase(), HctxPhase::Recovering);
    assert_ne!(recovering.epoch(), draining_epoch);
    assert!(!control.accepts_event(draining_epoch));
}

#[test]
fn staged_request_waiting_for_an_inflight_irq_does_not_spin_worker() {
    let waiting_for_irq = ServiceContinuation {
        cause_pending: false,
        dispatch_budget_exhausted: false,
        staged_request: true,
        inflight_request: true,
    };
    let exhausted_batch = ServiceContinuation {
        dispatch_budget_exhausted: true,
        ..waiting_for_irq
    };

    assert!(!waiting_for_irq.requires_immediate_requeue());
    assert!(exhausted_batch.requires_immediate_requeue());
}

#[test]
fn recovery_access_gate_closes_before_waiting_for_driver_borrows() {
    let gate = HctxAccessGate::new();
    let first = gate.try_enter().expect("running hctx accepts one accessor");

    assert!(!gate.close(), "an active driver borrow must delay recovery");
    assert!(gate.try_enter().is_none(), "closed hctx rejects new access");
    assert!(
        gate.leave(first),
        "last accessor transfers the recovery baton"
    );
    assert!(gate.is_drained());
    gate.reopen()
        .expect("reinitialization reopens a drained gate");
    let reopened = gate.try_enter().expect("reinitialized hctx accepts access");
    assert!(!gate.leave(reopened));
}
