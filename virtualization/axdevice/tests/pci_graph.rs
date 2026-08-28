use std::sync::Arc;

use axdevice::*;

struct EmptyModel {
    requirements: DeviceRequirements,
}

impl DeviceModel for EmptyModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        Ok(self.requirements.clone())
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

fn id(value: &str) -> DeviceNodeId {
    DeviceNodeId::new(value).unwrap()
}

fn slot(value: &str) -> ResourceSlot {
    ResourceSlot::new(value).unwrap()
}

fn host_key() -> PciHostKey {
    PciHostKey::new("primary-pci").unwrap()
}

fn endpoint_requirement() -> PciFunctionRequirement {
    PciFunctionRequirement::new(
        host_key(),
        PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0xff, 0, 0)),
    )
    .with_bar(PciMemoryBar::new(PciBarIndex::new(2).unwrap(), 0x1_0000).unwrap())
    .unwrap()
}

fn host_provider() -> PciHostProvider {
    let requirements = DeviceRequirements::new()
        .with_mmio(
            slot("pci-memory"),
            0x10_0000,
            0x10_0000,
            ResourceRequest::Auto,
        )
        .unwrap();
    let model = Arc::new(EmptyModel { requirements });
    PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::virtual_device(id("pci-host"), model),
        slot("pci-memory"),
    )
}

fn fixed_bdf(device: u8) -> PciBdf {
    PciBdf::new(PciSegment::new(0), 0, device, 0).unwrap()
}

fn endpoint_node(name: &str) -> DeviceNodeSpec {
    let requirements = DeviceRequirements::new()
        .with_pci_function(endpoint_requirement())
        .unwrap();
    DeviceNodeSpec::virtual_device(id(name), Arc::new(EmptyModel { requirements }))
}

fn pools() -> ResourcePools {
    let mut pools = ResourcePools::new();
    pools.add_auto_mmio(0xc000_0000..0xd000_0000).unwrap();
    pools
}

#[test]
fn pci_requirement_requires_a_registered_typed_host() {
    let mut graph = DeviceGraphBuilder::new();
    graph.add(endpoint_node("endpoint")).unwrap();

    let error = match graph.declare() {
        Ok(_) => panic!("an endpoint without a provider must fail declaration"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        DeviceGraphError::PciHostUnavailable { endpoint, .. } if endpoint == "endpoint"
    ));
}

#[test]
fn pci_endpoint_without_a_runtime_model_is_rejected_before_resolution() {
    let requirements = DeviceRequirements::new()
        .with_pci_function(endpoint_requirement())
        .unwrap();
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();
    graph
        .add(DeviceNodeSpec::host_passthrough(
            id("passthrough"),
            requirements,
        ))
        .unwrap();

    let error = match graph.declare() {
        Ok(_) => panic!("a PCI endpoint without a runtime model must fail declaration"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        DeviceGraphError::PciEndpointRequiresRuntimeModel { node } if node == "passthrough"
    ));
}

#[test]
fn pci_host_provider_without_a_runtime_model_is_rejected_before_resolution() {
    let mut graph = DeviceGraphBuilder::new();
    let provider = PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::host_passthrough(id("pci-host"), DeviceRequirements::new()),
        slot("pci-memory"),
    );

    let error = match graph.register_pci_host(provider) {
        Ok(()) => panic!("a PCI host provider without a runtime model must fail registration"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        DeviceGraphError::PciHostRequiresRuntimeModel { node } if node == "pci-host"
    ));
}

#[test]
fn duplicate_pci_host_keys_are_rejected_before_the_second_node_is_added() {
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();

    assert!(matches!(
        graph.register_pci_host(host_provider()).unwrap_err(),
        DeviceGraphError::DuplicatePciHost { .. }
    ));
}

#[test]
fn declaration_adds_endpoint_to_host_dependency_and_freezes_topology_metadata() {
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();
    graph.add(endpoint_node("endpoint")).unwrap();

    let resolved = graph.declare().unwrap().resolve(pools()).unwrap();
    let endpoint = resolved
        .nodes()
        .find(|node| node.id().as_str() == "endpoint")
        .unwrap();
    assert_eq!(endpoint.dependencies(), &[id("pci-host")]);

    let topology = resolved.pci_topology(&host_key()).unwrap();
    let function = topology.function(&id("endpoint")).unwrap();
    assert_eq!(
        function.bdf(),
        PciBdf::new(PciSegment::new(0), 0, 0, 0).unwrap()
    );
    assert_eq!(function.owner(), &id("endpoint"));
    assert_eq!(function.host(), &id("pci-host"));
}

#[test]
fn resource_failure_does_not_publish_a_partially_resolved_graph() {
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();
    graph.add(endpoint_node("endpoint")).unwrap();

    assert!(
        graph
            .declare()
            .unwrap()
            .resolve(ResourcePools::new())
            .is_err()
    );
}

#[test]
fn platform_functions_are_owned_by_the_host_and_reserve_their_bdfs() {
    let host_function = PciFunctionSpec::new(
        id("q35-host-function"),
        PciEndpointIdentity::new(0x8086, 0x29c0, PciClass::new(0x06, 0, 0)),
    )
    .with_bdf(ResourceRequest::Fixed(fixed_bdf(0)));
    let provider = host_provider()
        .with_platform_function(host_function)
        .unwrap();
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(provider).unwrap();
    graph.add(endpoint_node("endpoint")).unwrap();

    let resolved = graph.declare().unwrap().resolve(pools()).unwrap();
    let topology = resolved.pci_topology(&host_key()).unwrap();
    let platform = topology.function(&id("q35-host-function")).unwrap();
    let endpoint = topology.function(&id("endpoint")).unwrap();
    assert_eq!(platform.owner(), &id("pci-host"));
    assert_eq!(platform.host(), &id("pci-host"));
    assert_eq!(endpoint.bdf(), fixed_bdf(1));
}

#[test]
fn manually_declaring_the_automatic_host_edge_is_rejected() {
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();
    graph
        .add(endpoint_node("endpoint").with_dependency(id("pci-host")))
        .unwrap();

    let error = match graph.declare() {
        Ok(_) => panic!("a duplicate explicit and automatic edge must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        DeviceGraphError::DuplicateDependency { node, dependency }
            if node == "endpoint" && dependency == "pci-host"
    ));
}

#[test]
fn fixed_endpoint_cannot_claim_a_provider_reserved_bdf() {
    let provider = host_provider().with_reserved_bdf(fixed_bdf(3));
    let requirement = endpoint_requirement().with_bdf(ResourceRequest::Fixed(fixed_bdf(3)));
    let model = Arc::new(EmptyModel {
        requirements: DeviceRequirements::new()
            .with_pci_function(requirement)
            .unwrap(),
    });
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(provider).unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(id("endpoint"), model))
        .unwrap();

    let error = match graph.declare().unwrap().resolve(pools()) {
        Ok(_) => panic!("a reserved fixed BDF must fail resolution"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        DeviceManagerError::Pci(PciError::BdfReserved { function, .. })
            if function == "endpoint"
    ));
}

#[test]
fn automatic_endpoint_host_edge_participates_in_cycle_detection() {
    let requirements = DeviceRequirements::new()
        .with_mmio(
            slot("pci-memory"),
            0x10_0000,
            0x10_0000,
            ResourceRequest::Auto,
        )
        .unwrap();
    let host =
        DeviceNodeSpec::virtual_device(id("pci-host"), Arc::new(EmptyModel { requirements }))
            .with_dependency(id("endpoint"));
    let provider = PciHostProvider::new(host_key(), host, slot("pci-memory"));
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(provider).unwrap();
    graph.add(endpoint_node("endpoint")).unwrap();

    let error = match graph.declare() {
        Ok(_) => panic!("automatic PCI dependencies must participate in cycle checks"),
        Err(error) => error,
    };
    assert!(matches!(error, DeviceGraphError::DependencyCycle { .. }));
}

#[test]
fn duplicate_host_node_ids_fail_at_provider_registration() {
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();

    let secondary_requirements = DeviceRequirements::new()
        .with_mmio(
            slot("secondary-pci-memory"),
            0x10_0000,
            0x10_0000,
            ResourceRequest::Auto,
        )
        .unwrap();
    let secondary = PciHostProvider::new(
        PciHostKey::new("secondary-pci").unwrap(),
        // Reuses the primary host node id on purpose.
        DeviceNodeSpec::virtual_device(
            id("pci-host"),
            Arc::new(EmptyModel {
                requirements: secondary_requirements,
            }),
        ),
        slot("secondary-pci-memory"),
    );

    assert!(matches!(
        graph.register_pci_host(secondary),
        Err(DeviceGraphError::DuplicateNode { node })
            if node == "pci-host"
    ));
}

#[test]
fn multi_host_resolution_fails_atomically_when_one_aperture_cannot_plan() {
    fn secondary_provider() -> PciHostProvider {
        let requirements = DeviceRequirements::new()
            .with_mmio(
                slot("secondary-pci-memory"),
                // Larger than any pool this test grants, so planning for the
                // second host fails after the first one already resolved.
                0x4000_0000,
                0x4000_0000,
                ResourceRequest::Auto,
            )
            .unwrap();
        let model = Arc::new(EmptyModel { requirements });
        PciHostProvider::new(
            PciHostKey::new("secondary-pci").unwrap(),
            DeviceNodeSpec::virtual_device(id("pci-secondary"), model),
            slot("secondary-pci-memory"),
        )
    }

    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(host_provider()).unwrap();
    graph.register_pci_host(secondary_provider()).unwrap();

    let error = match graph.declare().unwrap().resolve(pools()) {
        Ok(_) => panic!("an unplannable second aperture must abort resolution"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("secondary-pci-memory"),
        "unexpected error: {error}"
    );
}
