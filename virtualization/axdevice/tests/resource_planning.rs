use axdevice::*;
use axdevice_base::*;

fn slot(name: &str) -> ResourceSlot {
    ResourceSlot::new(name).unwrap()
}

fn mmio_request(id: &str, request: ResourceRequest<u64>) -> DevicePlanRequest {
    DevicePlanRequest::new(
        id,
        DeviceRequirements::new()
            .with_mmio(slot("registers"), 0x100, 0x100, request)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn planning_is_fixed_first_deterministic_and_reports_exhaustion() {
    let pools = || {
        let mut pools = ResourcePools::new();
        pools.add_auto_mmio(0x1000..0x1400).unwrap();
        pools.allow_fixed_mmio(0x1000..0x1400).unwrap();
        pools
    };
    let left = VmResourcePlanner::new(pools())
        .plan([
            mmio_request("auto", ResourceRequest::Auto),
            mmio_request("fixed", ResourceRequest::Fixed(0x1000)),
        ])
        .unwrap();
    let right = VmResourcePlanner::new(pools())
        .plan([
            mmio_request("fixed", ResourceRequest::Fixed(0x1000)),
            mmio_request("auto", ResourceRequest::Auto),
        ])
        .unwrap();
    assert_eq!(
        left.resources("auto").unwrap(),
        right.resources("auto").unwrap()
    );
    assert_eq!(
        left.resources("fixed").unwrap().mmio(&slot("registers")),
        Ok((0x1000, 0x100))
    );
    assert_eq!(
        left.resources("auto").unwrap().mmio(&slot("registers")),
        Ok((0x1100, 0x100))
    );

    let mut exhausted = ResourcePools::new();
    exhausted.add_auto_mmio(0x1000..0x1080).unwrap();
    assert!(matches!(
        VmResourcePlanner::new(exhausted)
            .plan([mmio_request("large", ResourceRequest::Auto)])
            .unwrap_err(),
        ResourcePlanningError::Exhausted {
            namespace: ResourceNamespace::Mmio,
            ..
        }
    ));
}

fn irq_request(
    id: &str,
    controller: InterruptControllerId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
) -> DevicePlanRequest {
    DevicePlanRequest::new(
        id,
        DeviceRequirements::new()
            .with_wired_irq(
                slot("irq"),
                controller,
                trigger,
                sharing,
                ResourceRequest::Fixed(ControllerInputId::new(40)),
            )
            .unwrap(),
    )
    .unwrap()
}

fn irq_pools(controllers: &[InterruptControllerId]) -> ResourcePools {
    let mut pools = ResourcePools::new();
    for controller in controllers {
        pools
            .allow_fixed_controller_inputs(
                *controller,
                ControllerInputId::new(32)..ControllerInputId::new(64),
            )
            .unwrap();
    }
    pools
}

#[test]
fn wired_irq_namespaces_and_sharing_are_checked() {
    let left = InterruptControllerId::new(1);
    let right = InterruptControllerId::new(2);
    VmResourcePlanner::new(irq_pools(&[left, right]))
        .plan([
            irq_request(
                "left",
                left,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            irq_request(
                "right",
                right,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        ])
        .unwrap();

    let conflict = VmResourcePlanner::new(irq_pools(&[left]))
        .plan([
            irq_request(
                "alpha",
                left,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            irq_request(
                "beta",
                left,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        conflict,
        ResourcePlanningError::Conflict { existing_owner, requester, .. }
            if existing_owner == "alpha" && requester == "beta"
    ));

    assert!(
        VmResourcePlanner::new(irq_pools(&[left]))
            .plan([
                irq_request(
                    "edge",
                    left,
                    InterruptTrigger::EdgeTriggered,
                    InterruptSharing::Shared
                ),
                irq_request(
                    "level",
                    left,
                    InterruptTrigger::LevelTriggered,
                    InterruptSharing::Shared
                ),
            ])
            .is_err()
    );
}

#[test]
fn claim_drop_rolls_back_for_an_identical_retry() {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000..0x2000).unwrap();
    let plan = VmResourcePlanner::new(pools)
        .plan([mmio_request("uart", ResourceRequest::Auto)])
        .unwrap();

    let claims = plan.claim_device("uart").unwrap();
    assert!(plan.claim_device("uart").is_err());
    assert!(claims.finish().is_err());
    drop(claims);

    let mut retry = plan.claim_device("uart").unwrap();
    let lease = retry.consume(&slot("registers")).unwrap();
    retry.finish().unwrap();
    plan.verify_consumed().unwrap();
    drop(lease);
    assert!(plan.claim_device("uart").is_ok());
}

fn allow_msi(pools: &mut ResourcePools, controller: InterruptControllerId, its: ItsId) {
    pools
        .allow_fixed_msi_domain(
            controller,
            its,
            MsiDeviceId::new(0)..MsiDeviceId::new(32),
            MsiEventId::new(0)..MsiEventId::new(32),
            LpiId::new(8192)..LpiId::new(8256),
        )
        .unwrap();
}

fn msi_request(
    id: &str,
    controller: InterruptControllerId,
    its: ItsId,
    lpi: LpiId,
) -> DevicePlanRequest {
    let request = MsiResourceRequest::new(
        controller,
        its,
        4,
        ResourceRequest::Fixed(MsiDeviceId::new(3)),
        ResourceRequest::Fixed(MsiEventId::new(0)),
        ResourceRequest::Fixed(lpi),
    )
    .unwrap();
    DevicePlanRequest::new(
        id,
        DeviceRequirements::new()
            .with_msi(slot("msi"), request)
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn its_ids_are_isolated_but_lpis_are_controller_global() {
    let controller = InterruptControllerId::new(3);
    let left = ItsId::new(1);
    let right = ItsId::new(2);
    let pools = || {
        let mut pools = ResourcePools::new();
        allow_msi(&mut pools, controller, left);
        allow_msi(&mut pools, controller, right);
        pools
    };
    VmResourcePlanner::new(pools())
        .plan([
            msi_request("left", controller, left, LpiId::new(8192)),
            msi_request("right", controller, right, LpiId::new(8200)),
        ])
        .unwrap();
    assert!(matches!(
        VmResourcePlanner::new(pools())
            .plan([
                msi_request("left", controller, left, LpiId::new(8192)),
                msi_request("right", controller, right, LpiId::new(8192)),
            ])
            .unwrap_err(),
        ResourcePlanningError::Conflict {
            namespace: ResourceNamespace::Lpi(found),
            ..
        } if found == controller
    ));
}
