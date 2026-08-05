use axdevice::{
    DevicePlanRequest, DeviceRequirements, ResourceNamespace, ResourcePlanningError, ResourcePools,
    ResourceRequest, ResourceSlot, VmResourcePlanner,
};
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};

fn slot() -> ResourceSlot {
    ResourceSlot::new("irq").unwrap()
}

fn irq_device(
    id: &str,
    controller: InterruptControllerId,
    input: ControllerInputId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
) -> DevicePlanRequest {
    DevicePlanRequest::new(
        id,
        DeviceRequirements::new()
            .with_wired_irq(
                slot(),
                controller,
                trigger,
                sharing,
                ResourceRequest::Fixed(input),
            )
            .unwrap(),
    )
    .unwrap()
}

fn allow_input(pools: &mut ResourcePools, controller: InterruptControllerId) {
    pools
        .allow_fixed_controller_inputs(
            controller,
            ControllerInputId::new(32)..ControllerInputId::new(64),
        )
        .unwrap();
}

#[test]
fn same_input_number_on_different_controllers_does_not_conflict() {
    let left = InterruptControllerId::new(1);
    let right = InterruptControllerId::new(2);
    let mut pools = ResourcePools::new();
    allow_input(&mut pools, left);
    allow_input(&mut pools, right);

    let plan = VmResourcePlanner::new(pools)
        .plan([
            irq_device(
                "left",
                left,
                ControllerInputId::new(40),
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            irq_device(
                "right",
                right,
                ControllerInputId::new(40),
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        ])
        .unwrap();

    assert_eq!(
        plan.resources("left")
            .unwrap()
            .wired_irq(&slot())
            .unwrap()
            .controller(),
        left
    );
    assert_eq!(
        plan.resources("right")
            .unwrap()
            .wired_irq(&slot())
            .unwrap()
            .controller(),
        right
    );
}

#[test]
fn exclusive_conflict_reports_both_owners() {
    let controller = InterruptControllerId::new(3);
    let input = ControllerInputId::new(45);
    let mut pools = ResourcePools::new();
    allow_input(&mut pools, controller);
    let error = VmResourcePlanner::new(pools)
        .plan([
            irq_device(
                "alpha",
                controller,
                input,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            irq_device(
                "beta",
                controller,
                input,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        ])
        .unwrap_err();

    assert!(matches!(
        error,
        ResourcePlanningError::Conflict {
            namespace: ResourceNamespace::ControllerInput(found),
            existing_owner,
            requester,
            ..
        } if found == controller && existing_owner == "alpha" && requester == "beta"
    ));
}

#[test]
fn shared_inputs_require_matching_triggers() {
    let controller = InterruptControllerId::new(4);
    let input = ControllerInputId::new(46);
    let mut pools = ResourcePools::new();
    allow_input(&mut pools, controller);
    let error = VmResourcePlanner::new(pools)
        .plan([
            irq_device(
                "edge",
                controller,
                input,
                InterruptTrigger::EdgeTriggered,
                InterruptSharing::Shared,
            ),
            irq_device(
                "level",
                controller,
                input,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Shared,
            ),
        ])
        .unwrap_err();
    assert!(
        matches!(error, ResourcePlanningError::Conflict { detail, .. } if detail.contains("trigger"))
    );
}

#[test]
fn compatible_shared_level_inputs_are_accepted() {
    let controller = InterruptControllerId::new(5);
    let input = ControllerInputId::new(47);
    let mut pools = ResourcePools::new();
    allow_input(&mut pools, controller);
    let plan = VmResourcePlanner::new(pools)
        .plan([
            irq_device(
                "left",
                controller,
                input,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Shared,
            ),
            irq_device(
                "right",
                controller,
                input,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Shared,
            ),
        ])
        .unwrap();
    assert_eq!(
        plan.owner_of_controller_input(controller, input).as_deref(),
        Some("left")
    );
}
