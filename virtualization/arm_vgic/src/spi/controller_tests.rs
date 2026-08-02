use super::*;

fn intid(value: u32) -> ArmSpiIntId {
    ArmSpiIntId::new(value).unwrap()
}
fn target(value: u32) -> VgicVcpuId {
    VgicVcpuId::new(value)
}
fn controller(trigger: InterruptTriggerMode) -> ArmVgicController {
    ArmVgicController::new([(ArmSpiRoute::new(intid(32), target(0)), trigger)]).unwrap()
}
fn deliver(controller: &ArmVgicController) -> DeliveryDescriptor {
    let mut installed = None;
    controller
        .deliver_one(target(0), |descriptor| {
            installed = Some(descriptor);
            Ok::<_, ()>(())
        })
        .unwrap();
    installed.unwrap()
}
fn observe(descriptor: DeliveryDescriptor, state: ResidentLrState) -> ResidentObservation {
    ResidentObservation::new(
        target(0),
        descriptor.intid(),
        descriptor.epoch(),
        state,
        false,
    )
}

#[test]
fn validates_intid_boundaries_and_builder_phase() {
    assert!(ArmSpiIntId::new(31).is_err());
    assert!(ArmSpiIntId::new(32).is_ok());
    assert!(ArmSpiIntId::new(512).is_ok());
    assert!(ArmSpiIntId::new(1019).is_ok());
    assert!(ArmSpiIntId::new(1020).is_err());
    let mut builder = ArmVgicControllerBuilder::new();
    let controller = builder.controller();
    assert_eq!(controller.pulse(intid(32)), Err(VgicError::NotReady));
    builder
        .register(
            ArmSpiRoute::new(intid(32), target(0)),
            InterruptTriggerMode::EdgeTriggered,
        )
        .unwrap();
    assert!(matches!(
        builder.register(
            ArmSpiRoute::new(intid(32), target(1)),
            InterruptTriggerMode::EdgeTriggered
        ),
        Err(VgicError::DuplicateSpiRoute { .. })
    ));
    builder.finish().unwrap().pulse(intid(32)).unwrap();
}

#[test]
fn rejects_unregistered_sources_and_trigger_mismatches() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    assert!(matches!(
        controller.pulse(intid(33)),
        Err(VgicError::UnregisteredSpi { intid: 33 })
    ));
    assert!(matches!(
        controller.set_level(intid(32), true),
        Err(VgicError::TriggerMismatch { intid: 32, .. })
    ));
}

#[test]
fn failed_install_does_not_consume_pending_or_epoch() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    assert_eq!(
        controller.deliver_one(target(0), |_| Err::<(), _>(7)),
        Err(DeliveryError::Installer(7))
    );
    let descriptor = deliver(&controller);
    assert_eq!(descriptor.epoch().as_u64(), 1);
}

#[test]
fn edge_while_active_becomes_active_pending_without_replacing_epoch() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let descriptor = deliver(&controller);
    controller
        .fold(observe(descriptor, ResidentLrState::Active))
        .unwrap();
    assert_eq!(
        controller.pulse(intid(32)).unwrap(),
        ServiceHint::Target(target(0))
    );
    let mut update = None;
    assert_eq!(
        controller
            .reconcile(observe(descriptor, ResidentLrState::Active), |value| {
                update = Some(value);
                Ok::<_, ()>(())
            })
            .unwrap(),
        ReconcileOutcome::Updated
    );
    assert_eq!(
        update,
        Some(ResidentUpdate::SetState(ResidentLrState::ActivePending))
    );
    assert_eq!(
        controller.deliver_one(target(0), |_| Ok::<_, ()>(())),
        Ok(DeliveryOutcome::NoWork)
    );
}

#[test]
fn failed_reconcile_does_not_commit_the_planned_lr_update() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let descriptor = deliver(&controller);
    controller
        .fold(observe(descriptor, ResidentLrState::Active))
        .unwrap();
    controller.pulse(intid(32)).unwrap();

    assert_eq!(
        controller.reconcile(observe(descriptor, ResidentLrState::Active), |_| Err(7)),
        Err(ReconcileError::Apply(7))
    );
    let mut retry = None;
    controller
        .reconcile(observe(descriptor, ResidentLrState::Active), |update| {
            retry = Some(update);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        retry,
        Some(ResidentUpdate::SetState(ResidentLrState::ActivePending))
    );
}

#[test]
fn disabling_a_resident_edge_pending_preserves_it_for_reenable() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let descriptor = deliver(&controller);

    controller.set_enabled(intid(32), false).unwrap();
    assert_eq!(
        controller
            .reconcile(
                observe(descriptor, ResidentLrState::Pending),
                |_| Ok::<_, ()>(())
            )
            .unwrap(),
        ReconcileOutcome::Released
    );
    controller.set_enabled(intid(32), true).unwrap();

    assert!(matches!(
        controller.deliver_one(target(0), |_| Ok::<_, ()>(())),
        Ok(DeliveryOutcome::Installed { intid: installed, .. }) if installed == intid(32)
    ));
}

#[test]
fn disabling_active_pending_preserves_its_pending_part_after_deactivation() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let descriptor = deliver(&controller);
    controller
        .fold(observe(descriptor, ResidentLrState::Active))
        .unwrap();
    controller.pulse(intid(32)).unwrap();
    controller
        .reconcile(observe(descriptor, ResidentLrState::Active), |_| {
            Ok::<_, ()>(())
        })
        .unwrap();

    controller.set_enabled(intid(32), false).unwrap();
    controller.request_deactivation(intid(32)).unwrap();
    controller
        .reconcile(observe(descriptor, ResidentLrState::ActivePending), |_| {
            Ok::<_, ()>(())
        })
        .unwrap();
    controller
        .reconcile(observe(descriptor, ResidentLrState::Pending), |_| {
            Ok::<_, ()>(())
        })
        .unwrap();
    controller.set_enabled(intid(32), true).unwrap();

    assert!(matches!(
        controller.deliver_one(target(0), |_| Ok::<_, ()>(())),
        Ok(DeliveryOutcome::Installed { intid: installed, .. }) if installed == intid(32)
    ));
}

#[test]
fn deactivating_active_pending_leaves_one_pending_instance() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let descriptor = deliver(&controller);
    controller
        .fold(observe(descriptor, ResidentLrState::Active))
        .unwrap();
    controller.pulse(intid(32)).unwrap();
    controller
        .reconcile(observe(descriptor, ResidentLrState::Active), |_| {
            Ok::<_, ()>(())
        })
        .unwrap();

    assert_eq!(
        controller.request_deactivation(intid(32)).unwrap(),
        ServiceHint::Target(target(0))
    );
    assert_eq!(
        controller
            .reconcile(
                observe(descriptor, ResidentLrState::ActivePending),
                |_| Ok::<_, ()>(())
            )
            .unwrap(),
        ReconcileOutcome::Updated
    );
    assert_eq!(
        controller.request_deactivation(intid(32)).unwrap(),
        ServiceHint::None
    );
    assert_eq!(
        controller
            .reconcile(
                observe(descriptor, ResidentLrState::Pending),
                |_| -> Result<(), ()> {
                    panic!("a completed command must not rewrite a pending LR")
                }
            )
            .unwrap(),
        ReconcileOutcome::Resident
    );
}

#[test]
fn level_lower_revokes_pending_but_not_active() {
    let controller = controller(InterruptTriggerMode::LevelTriggered);
    controller.set_level(intid(32), true).unwrap();
    let first = deliver(&controller);
    controller.set_level(intid(32), false).unwrap();
    assert_eq!(
        controller
            .reconcile(
                observe(first, ResidentLrState::Pending),
                |_| Ok::<_, ()>(())
            )
            .unwrap(),
        ReconcileOutcome::Released
    );

    controller.set_level(intid(32), true).unwrap();
    let second = deliver(&controller);
    controller
        .fold(observe(second, ResidentLrState::Active))
        .unwrap();
    controller.set_level(intid(32), false).unwrap();
    assert_eq!(
        controller
            .reconcile(
                observe(second, ResidentLrState::Active),
                |_| -> Result<(), ()> { panic!("active LR must stay resident") }
            )
            .unwrap(),
        ReconcileOutcome::Resident
    );
}

#[test]
fn asserted_level_requeues_after_deactivation() {
    let controller = controller(InterruptTriggerMode::LevelTriggered);
    controller.set_level(intid(32), true).unwrap();
    let first = deliver(&controller);
    controller
        .fold(observe(first, ResidentLrState::Active))
        .unwrap();
    assert_eq!(
        controller.request_deactivation(intid(32)).unwrap(),
        ServiceHint::Target(target(0))
    );
    assert_eq!(
        controller
            .reconcile(observe(first, ResidentLrState::Active), |_| Ok::<_, ()>(()))
            .unwrap(),
        ReconcileOutcome::Released
    );
    let second = deliver(&controller);
    assert!(second.epoch() > first.epoch());
}

#[test]
fn stale_observation_cannot_release_new_delivery() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.pulse(intid(32)).unwrap();
    let first = deliver(&controller);
    controller
        .fold(observe(first, ResidentLrState::Invalid))
        .unwrap();
    controller.pulse(intid(32)).unwrap();
    let second = deliver(&controller);
    assert!(matches!(
        controller.fold(observe(first, ResidentLrState::Invalid)),
        Err(VgicError::ResidentMismatch { .. })
    ));
    assert_eq!(
        controller
            .fold(observe(second, ResidentLrState::Pending))
            .unwrap(),
        FoldOutcome::Resident
    );
}

#[test]
fn enabling_a_pending_source_returns_target_hint() {
    let controller = controller(InterruptTriggerMode::EdgeTriggered);
    controller.set_enabled(intid(32), false).unwrap();
    assert_eq!(controller.pulse(intid(32)).unwrap(), ServiceHint::None);
    assert_eq!(
        controller.set_enabled(intid(32), true).unwrap(),
        ServiceHint::Target(target(0))
    );
}
