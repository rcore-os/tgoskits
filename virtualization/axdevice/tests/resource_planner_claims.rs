use axdevice::{
    DevicePlanRequest, DeviceRequirements, ResourcePools, ResourceRequest, ResourceSlot,
    VmResourcePlanner,
};

fn slot(name: &str) -> ResourceSlot {
    ResourceSlot::new(name).unwrap()
}

fn plan() -> axdevice::VmResourcePlan {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000..0x2000).unwrap();
    let requirements = DeviceRequirements::new()
        .with_mmio(slot("registers"), 0x100, 0x100, ResourceRequest::Auto)
        .unwrap();
    VmResourcePlanner::new(pools)
        .plan([DevicePlanRequest::new("uart", requirements).unwrap()])
        .unwrap()
}

#[test]
fn claims_are_one_shot_while_issued() {
    let plan = plan();
    let claims = plan.claim_device("uart").unwrap();
    assert!(plan.claim_device("uart").is_err());
    drop(claims);
    assert!(plan.claim_device("uart").is_ok());
}

#[test]
fn unconsumed_claims_prevent_commit_and_roll_back_on_drop() {
    let plan = plan();
    {
        let claims = plan.claim_device("uart").unwrap();
        assert_eq!(claims.remaining(), 1);
        assert!(claims.finish().is_err());
        assert!(plan.verify_consumed().is_err());
    }
    assert!(plan.claim_device("uart").is_ok());
}

#[test]
fn a_dropped_lease_makes_the_same_resource_retryable() {
    let plan = plan();
    let mut claims = plan.claim_device("uart").unwrap();
    let lease = claims.consume(&slot("registers")).unwrap();
    claims.finish().unwrap();
    plan.verify_consumed().unwrap();
    drop(lease);

    let mut retry = plan.claim_device("uart").unwrap();
    let retry_lease = retry.consume(&slot("registers")).unwrap();
    retry.finish().unwrap();
    plan.verify_consumed().unwrap();
    drop(retry_lease);
}

#[test]
fn duplicate_devices_are_rejected_before_allocation() {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0x1000..0x2000).unwrap();
    let request = DevicePlanRequest::new(
        "uart",
        DeviceRequirements::new()
            .with_mmio(slot("registers"), 0x100, 0x100, ResourceRequest::Auto)
            .unwrap(),
    )
    .unwrap();
    assert!(
        VmResourcePlanner::new(pools)
            .plan([request.clone(), request])
            .is_err()
    );
}

#[test]
fn duplicate_slots_are_rejected_by_the_model() {
    let requirements = DeviceRequirements::new()
        .with_mmio(slot("registers"), 0x100, 0x100, ResourceRequest::Auto)
        .unwrap();
    assert!(
        requirements
            .with_mmio(slot("registers"), 0x100, 0x100, ResourceRequest::Auto)
            .is_err()
    );
}
