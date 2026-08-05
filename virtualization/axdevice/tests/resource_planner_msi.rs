use axdevice::{
    DevicePlanRequest, DeviceRequirements, MsiResourceRequest, ResourceNamespace,
    ResourcePlanningError, ResourcePools, ResourceRequest, ResourceSlot, VmResourcePlanner,
};
use axdevice_base::{InterruptControllerId, ItsId, LpiId, MsiDeviceId, MsiEventId};

fn slot() -> ResourceSlot {
    ResourceSlot::new("msi").unwrap()
}

fn msi_device(
    id: &str,
    controller: InterruptControllerId,
    its: ItsId,
    device: MsiDeviceId,
    lpi: LpiId,
) -> DevicePlanRequest {
    let msi = MsiResourceRequest::new(
        controller,
        its,
        4,
        ResourceRequest::Fixed(device),
        ResourceRequest::Fixed(MsiEventId::new(0)),
        ResourceRequest::Fixed(lpi),
    )
    .unwrap();
    DevicePlanRequest::new(id, DeviceRequirements::new().with_msi(slot(), msi).unwrap()).unwrap()
}

fn allow_domain(pools: &mut ResourcePools, controller: InterruptControllerId, its: ItsId) {
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

#[test]
fn device_ids_are_isolated_by_its() {
    let controller = InterruptControllerId::new(1);
    let left = ItsId::new(1);
    let right = ItsId::new(2);
    let mut pools = ResourcePools::new();
    allow_domain(&mut pools, controller, left);
    allow_domain(&mut pools, controller, right);
    let plan = VmResourcePlanner::new(pools)
        .plan([
            msi_device(
                "left",
                controller,
                left,
                MsiDeviceId::new(3),
                LpiId::new(8192),
            ),
            msi_device(
                "right",
                controller,
                right,
                MsiDeviceId::new(3),
                LpiId::new(8200),
            ),
        ])
        .unwrap();
    assert_eq!(
        plan.resources("left")
            .unwrap()
            .msi(&slot())
            .unwrap()
            .device(),
        MsiDeviceId::new(3)
    );
    assert_eq!(
        plan.resources("right")
            .unwrap()
            .msi(&slot())
            .unwrap()
            .device(),
        MsiDeviceId::new(3)
    );
}

#[test]
fn lpis_conflict_across_its_on_the_same_controller() {
    let controller = InterruptControllerId::new(1);
    let left = ItsId::new(1);
    let right = ItsId::new(2);
    let mut pools = ResourcePools::new();
    allow_domain(&mut pools, controller, left);
    allow_domain(&mut pools, controller, right);
    let error = VmResourcePlanner::new(pools)
        .plan([
            msi_device(
                "left",
                controller,
                left,
                MsiDeviceId::new(3),
                LpiId::new(8192),
            ),
            msi_device(
                "right",
                controller,
                right,
                MsiDeviceId::new(3),
                LpiId::new(8192),
            ),
        ])
        .unwrap_err();
    assert!(matches!(
        error,
        ResourcePlanningError::Conflict {
            namespace: ResourceNamespace::Lpi(found),
            ..
        } if found == controller
    ));
}
