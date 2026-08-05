use axdevice::{
    DevicePlanRequest, DeviceRequirements, ResourceNamespace, ResourcePlanningError, ResourcePools,
    ResourceRequest, ResourceSlot, VmResourcePlanner,
};

fn slot(name: &str) -> ResourceSlot {
    ResourceSlot::new(name).unwrap()
}

fn mmio_device(id: &str, request: ResourceRequest<u64>) -> DevicePlanRequest {
    DevicePlanRequest::new(
        id,
        DeviceRequirements::new()
            .with_mmio(slot("registers"), 0x100, 0x100, request)
            .unwrap(),
    )
    .unwrap()
}

fn address_pools() -> ResourcePools {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000..0x1400).unwrap();
    pools.allow_fixed_mmio(0x1000..0x1400).unwrap();
    pools
}

#[test]
fn fixed_requests_are_reserved_before_automatic_requests() {
    let plan = VmResourcePlanner::new(address_pools())
        .plan([
            mmio_device("auto", ResourceRequest::Auto),
            mmio_device("fixed", ResourceRequest::Fixed(0x1000)),
        ])
        .unwrap();

    assert_eq!(
        plan.resources("fixed").unwrap().mmio(&slot("registers")),
        Ok((0x1000, 0x100))
    );
    assert_eq!(
        plan.resources("auto").unwrap().mmio(&slot("registers")),
        Ok((0x1100, 0x100))
    );
}

#[test]
fn allocation_is_independent_of_input_order() {
    let request_a = mmio_device("a", ResourceRequest::Auto);
    let request_b = mmio_device("b", ResourceRequest::Auto);
    let left = VmResourcePlanner::new(address_pools())
        .plan([request_b.clone(), request_a.clone()])
        .unwrap();
    let right = VmResourcePlanner::new(address_pools())
        .plan([request_a, request_b])
        .unwrap();

    assert_eq!(left.resources("a").unwrap(), right.resources("a").unwrap());
    assert_eq!(left.resources("b").unwrap(), right.resources("b").unwrap());
    assert_eq!(
        left.resources("a").unwrap().mmio(&slot("registers")),
        Ok((0x1000, 0x100))
    );
}

#[test]
fn reservations_and_alignment_are_enforced() {
    let mut pools = address_pools();
    pools.reserve_mmio("platform", 0x1000..0x1100).unwrap();
    let plan = VmResourcePlanner::new(pools)
        .plan([mmio_device("auto", ResourceRequest::Auto)])
        .unwrap();
    assert_eq!(
        plan.resources("auto").unwrap().mmio(&slot("registers")),
        Ok((0x1100, 0x100))
    );

    let error = VmResourcePlanner::new(address_pools())
        .plan([mmio_device("bad", ResourceRequest::Fixed(0x1080))])
        .unwrap_err();
    assert!(matches!(
        error,
        ResourcePlanningError::FixedNotAllowed {
            namespace: ResourceNamespace::Mmio,
            ..
        }
    ));
}

#[test]
fn address_exhaustion_is_structured() {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000..0x1080).unwrap();
    let error = VmResourcePlanner::new(pools)
        .plan([mmio_device("too-large", ResourceRequest::Auto)])
        .unwrap_err();
    assert!(matches!(
        error,
        ResourcePlanningError::Exhausted {
            namespace: ResourceNamespace::Mmio,
            ..
        }
    ));
}
