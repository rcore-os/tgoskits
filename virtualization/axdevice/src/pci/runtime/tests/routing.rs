use super::*;

#[test]
fn binding_rejects_an_unsupported_config_effect_before_publishing_route() {
    let effect = PciCapabilityEffectRegion::new(
        PciConfigEffectId::new(7),
        2,
        1,
        PciCapabilityEffectAccess::ReadWrite,
    )
    .unwrap();
    let capability =
        PciCapabilitySpec::new(PciCapabilityId::new(9), alloc::vec![0], alloc::vec![0])
            .unwrap()
            .with_effect(effect)
            .unwrap();
    let function_id = DeviceNodeId::new("unsupported-effect-endpoint").unwrap();
    let mut builder = PciTopologyBuilder::new();
    builder
        .add_function(
            PciFunctionSpec::new(
                function_id.clone(),
                PciEndpointIdentity::new(0x1af4, 0x1041, PciClass::new(0xff, 0, 0)),
            )
            .with_capability(capability),
        )
        .unwrap();
    let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
    let bdf = topology.function(&function_id).unwrap().bdf();
    let binding = Arc::new(PciRootBinding::new(
        DeviceNodeId::new("host").unwrap(),
        Arc::new(PciRootState::new(topology)),
    ));
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let mut grants = Vec::new();

    assert!(matches!(
        binding.bind_registered(&function_id, DeviceId::new(7), function, &mut grants),
        Err(DeviceManagerError::InvalidConfig { .. })
    ));
    assert!(grants.is_empty());
    assert!(matches!(
        binding.read_config(bdf, ConfigOffset::new(0x42).unwrap(), AccessWidth::Byte,),
        Err(DeviceError::InvalidInput { .. })
    ));
}

#[test]
fn rebind_mints_a_new_generation_and_rejects_stale_tokens() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let device = DeviceId::new(7);

    let first = router.activate(device, Arc::clone(&function)).unwrap();
    assert_eq!(first.binding_generation(), 1);
    assert!(router.endpoint(&first).is_ok());

    let removed = router.invalidate(&first).unwrap();
    assert!(Arc::ptr_eq(&removed, &function));
    let second = router.activate(device, Arc::clone(&function)).unwrap();
    assert_eq!(second.binding_generation(), 2);

    // The old generation can never dispatch again, before or after the
    // new binding exists.
    assert!(matches!(
        router.endpoint(&first),
        Err(DeviceError::InvalidState { .. })
    ));
    drop(router.endpoint(&second).unwrap());
}

#[test]
fn invalidate_returns_none_for_unknown_or_stale_tokens() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let device = DeviceId::new(3);

    let token = router.activate(device, Arc::clone(&function)).unwrap();
    let forged = EndpointRouteToken {
        binding_generation: EndpointBindingGeneration(token.binding_generation().saturating_add(1)),
        ..token.clone()
    };
    assert!(router.invalidate(&forged).is_none());
    assert!(router.endpoint(&token).is_ok());
    assert_eq!(
        router.invalidate(&token).map(|arc| Arc::strong_count(&arc)),
        Some(2)
    );
    assert!(router.invalidate(&token).is_none());
}

#[test]
fn invalidation_closes_new_irq_permits_but_does_not_revoke_an_acquired_one() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let token = router
        .activate(DeviceId::new(4), function)
        .expect("test route activation succeeds");
    let permit = token
        .admission
        .clone()
        .acquire_irq_permit()
        .expect("permit is admitted before teardown");

    drop(router.invalidate(&token));
    assert!(matches!(
        token.admission.clone().acquire_irq_permit(),
        Err(DeviceError::InvalidState { .. })
    ));
    drop(permit);
}

#[test]
fn acquired_route_lease_keeps_grant_admitted_after_admission_close() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let token = router
        .activate(DeviceId::new(4), function)
        .expect("test route activation succeeds");
    let lease = router
        .lease(&token, true)
        .expect("route lease is admitted before reset");
    let retained = lease.grant.clone();

    assert!(router.invalidate(&token).is_some());

    assert!(lease.grant.admission_is_open());
    drop(lease);
    assert!(!retained.admission_is_open());
}

#[test]
fn acquired_route_lease_enters_nested_context_after_admission_close() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let token = router
        .activate(DeviceId::new(4), function)
        .expect("test route activation succeeds");
    let lease = router
        .lease(&token, true)
        .expect("route lease is admitted before reset");
    let grant = lease.grant.clone();
    let mut runtime = crate::DeviceRuntime::empty();

    assert!(router.invalidate(&token).is_some());

    runtime.with_routed_grant_for_test(0, grant, |context| {
        let mut callback = |_nested: &mut dyn DeviceContext| Ok(());
        context
            .with_routed_device(&lease.grant, &mut callback)
            .expect("an admitted route lease must survive admission close");
    });
    drop(lease);
}

#[test]
fn irq_permit_drain_has_a_bounded_failure_path() {
    let router = router();
    let function: Arc<dyn PciFunction> = Arc::new(StubFunction {
        fail_command: false,
    });
    let token = router
        .activate(DeviceId::new(4), function)
        .expect("test route activation succeeds");
    let permit = token
        .admission
        .clone()
        .acquire_irq_permit()
        .expect("permit is admitted before drain");

    assert!(matches!(
        token.admission.wait_for_irq_permits_with_budget(0),
        Err(DeviceManagerError::InvalidState { .. })
    ));
    drop(permit);
    token.admission.wait_for_irq_permits_with_budget(0).unwrap();
}
