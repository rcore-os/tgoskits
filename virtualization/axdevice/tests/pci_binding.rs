use std::sync::{Arc, Mutex};

use axdevice::*;
use axdevice_base::*;

const APERTURE_BASE: u64 = 0xc000_0000;
const APERTURE_SIZE: u64 = 0x10_0000;

type RootSlot = Arc<Mutex<Option<Arc<PciRootState>>>>;
type BindingSlot = Arc<Mutex<Option<Arc<PciRootBinding>>>>;

fn id(value: &str) -> DeviceNodeId {
    DeviceNodeId::new(value).unwrap()
}
fn slot(value: &str) -> ResourceSlot {
    ResourceSlot::new(value).unwrap()
}
fn host_key() -> PciHostKey {
    PciHostKey::new("pci").unwrap()
}

/// Deliberate host-bundle defects exercised by the registration tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostMode {
    Correct,
    NoService,
    DuplicateService,
    WrongOwner,
    ForeignTopology,
}

struct HostModel {
    root: RootSlot,
    binding: BindingSlot,
    mode: HostMode,
}

impl HostModel {
    /// Builds an unrelated topology so a published binding fails the
    /// resolved-topology identity check.
    fn foreign_binding() -> Arc<PciRootBinding> {
        let (graph, ..) = resolved_graph(RecordingEndpoint::shared(), false);
        let runtime = build_runtime(&graph);
        runtime
            .services()
            .all::<PciRootBindingKey>()
            .into_iter()
            .next()
            .expect("resolved fixture publishes one PCI root")
    }
}

impl DeviceModel for HostModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            slot("pci-memory"),
            APERTURE_SIZE,
            APERTURE_SIZE,
            ResourceRequest::Auto,
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let _aperture = context.mmio("pci-memory")?;
        let topology = context.pci_host_topology().unwrap().clone();
        let root = Arc::new(PciRootState::new(topology));
        let owner = match self.mode {
            HostMode::WrongOwner => id("other-host"),
            _ => id("pci-host"),
        };
        let binding = match self.mode {
            HostMode::ForeignTopology => Self::foreign_binding(),
            _ => Arc::new(PciRootBinding::new(owner, root.clone())),
        };
        match self.mode {
            HostMode::NoService => Ok(DeviceBundle::new()),
            HostMode::DuplicateService => {
                let bundle = DeviceBundle::new();
                let bundle = bundle.with_service::<PciRootBindingKey>(binding.clone())?;
                let bundle = bundle.with_service::<PciRootBindingKey>(binding)?;
                Ok(bundle)
            }
            HostMode::Correct | HostMode::WrongOwner | HostMode::ForeignTopology => {
                *self.root.lock().unwrap() = Some(root);
                *self.binding.lock().unwrap() = Some(binding.clone());
                let bundle = DeviceBundle::new().with_service::<PciRootBindingKey>(binding)?;
                Ok(bundle)
            }
        }
    }
}

#[derive(Default)]
struct RecordingEndpoint {
    reads: Mutex<Vec<(DeviceId, PciBarAccess)>>,
}

impl RecordingEndpoint {
    fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl Device for RecordingEndpoint {
    fn name(&self) -> &str {
        "recording-pci-endpoint"
    }
    fn resources(&self) -> &[Resource] {
        &[]
    }
    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Err(DeviceError::NotFound)
    }
    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Err(DeviceError::NotFound)
    }
}

impl PciFunction for RecordingEndpoint {
    fn read_bar(&self, access: PciBarAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        self.reads
            .lock()
            .unwrap()
            .push((context.device_id(), access));
        Ok(0xfeed_0000 | access.offset())
    }

    fn write_bar(
        &self,
        _access: PciBarAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

/// Declares the PCI requirement but builds no bundled function.
struct HeadlessEndpointModel;

impl DeviceModel for HeadlessEndpointModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        endpoint_requirements()
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Declares no PCI requirement yet bundles a PCI function anyway.
struct UndeclaredFunctionModel {
    function: Arc<dyn PciFunction>,
}

impl DeviceModel for UndeclaredFunctionModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        Ok(DeviceRequirements::new())
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let mut bundle = DeviceBundle::new();
        bundle.add_pci_function(self.function.clone())?;
        Ok(bundle)
    }
}

/// Publishes a PCI root from an ordinary node that owns no PCI metadata.
struct PlainRootModel {
    binding: BindingSlot,
}

impl DeviceModel for PlainRootModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        Ok(DeviceRequirements::new())
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let binding = HostModel::foreign_binding();
        *self.binding.lock().unwrap() = Some(binding.clone());
        let bundle = DeviceBundle::new().with_service::<PciRootBindingKey>(binding)?;
        Ok(bundle)
    }
}

struct ConflictingDevice(&'static str);

impl Device for ConflictingDevice {
    fn name(&self) -> &str {
        self.0
    }

    fn resources(&self) -> &[Resource] {
        static RESOURCES: [Resource; 1] = [Resource::MmioRange {
            base: 0x5000_0000,
            size: 0x1000,
        }];
        &RESOURCES
    }

    fn read(&self, _access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        Ok(0)
    }

    fn write(
        &self,
        _access: &DeviceAccess,
        _value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        Ok(())
    }
}

fn endpoint_requirements() -> DeviceManagerResult<DeviceRequirements> {
    let requirement = PciFunctionRequirement::new(
        host_key(),
        PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0xff, 0, 0)),
    )
    .with_bar(PciMemoryBar::new(PciBarIndex::new(2).unwrap(), 0x1_0000)?)?;
    DeviceRequirements::new().with_pci_function(requirement)
}

struct EndpointModel {
    endpoint: Arc<RecordingEndpoint>,
    fail_registration: bool,
}

impl DeviceModel for EndpointModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        endpoint_requirements()
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, _context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let mut bundle = DeviceBundle::new();
        bundle.add_pci_function(self.endpoint.clone())?;
        if self.fail_registration {
            bundle.add_device(Arc::new(ConflictingDevice("first")));
            bundle.add_device(Arc::new(ConflictingDevice("second")));
        }
        Ok(bundle)
    }
}

fn host_provider(root: RootSlot, binding: BindingSlot) -> PciHostProvider {
    PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::virtual_device(
            id("pci-host"),
            Arc::new(HostModel {
                root,
                binding,
                mode: HostMode::Correct,
            }),
        ),
        slot("pci-memory"),
    )
}

fn test_pools() -> ResourcePools {
    let mut pools = ResourcePools::new();
    pools
        .add_auto_mmio(APERTURE_BASE..APERTURE_BASE + APERTURE_SIZE)
        .unwrap();
    pools
}

fn resolved_graph(
    endpoint: Arc<RecordingEndpoint>,
    fail_registration: bool,
) -> (ResolvedDeviceGraph, RootSlot, BindingSlot) {
    resolved_graph_with_modes(endpoint, fail_registration, HostMode::Correct, false)
}

fn resolved_graph_with_modes(
    endpoint: Arc<RecordingEndpoint>,
    fail_registration: bool,
    host_mode: HostMode,
    headless_endpoint: bool,
) -> (ResolvedDeviceGraph, RootSlot, BindingSlot) {
    let root = Arc::new(Mutex::new(None));
    let binding = Arc::new(Mutex::new(None));
    let host_model = Arc::new(HostModel {
        root: root.clone(),
        binding: binding.clone(),
        mode: host_mode,
    });
    let endpoint_model: Arc<dyn DeviceModel> = if headless_endpoint {
        Arc::new(HeadlessEndpointModel)
    } else {
        Arc::new(EndpointModel {
            endpoint,
            fail_registration,
        })
    };
    let provider = PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::virtual_device(id("pci-host"), host_model),
        slot("pci-memory"),
    );
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(provider).unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("endpoint"),
            endpoint_model,
        ))
        .unwrap();
    (
        graph.declare().unwrap().resolve(test_pools()).unwrap(),
        root,
        binding,
    )
}

fn try_build_runtime(graph: &ResolvedDeviceGraph) -> DeviceManagerResult<DeviceRuntime> {
    let mut builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
    for node in graph.nodes() {
        builder.build_graph_node(node, graph.resource_plan())?;
    }
    builder.finish(graph.resource_plan())
}

fn build_runtime(graph: &ResolvedDeviceGraph) -> DeviceRuntime {
    try_build_runtime(graph).unwrap()
}

/// Builds a runtime that must fail, returning the rendered error message.
fn expect_build_error(graph: &ResolvedDeviceGraph) -> String {
    match try_build_runtime(graph) {
        Ok(_) => panic!("runtime registration must fail for this fixture"),
        Err(error) => error.to_string(),
    }
}

fn enable_mse(root: &PciRootState, function: &ResolvedPciFunction) {
    root.write_config(
        function.bdf(),
        ConfigOffset::new(4).unwrap(),
        AccessWidth::Word,
        2,
    )
    .unwrap();
}

fn endpoint_bar<'a>(
    graph: &'a ResolvedDeviceGraph,
    name: &str,
) -> (&'a ResolvedPciFunction, ResolvedPciBar) {
    let function = graph
        .pci_topology(&host_key())
        .unwrap()
        .function(&id(name))
        .unwrap();
    let bar = function.bar(PciBarIndex::new(2).unwrap()).unwrap();
    (function, bar)
}

#[test]
fn graph_bundles_bind_and_dispatch_with_the_endpoint_device_identity() {
    let endpoint = RecordingEndpoint::shared();
    let (graph, root_slot, binding_slot) = resolved_graph(endpoint.clone(), false);
    let runtime = build_runtime(&graph);
    let root = root_slot.lock().unwrap().clone().unwrap();
    let binding = binding_slot.lock().unwrap().clone().unwrap();
    let (function, bar) = endpoint_bar(&graph, "endpoint");
    enable_mse(&root, function);

    assert_eq!(
        binding
            .read_bar(bar.address() + 0x20, AccessWidth::Dword)
            .unwrap(),
        0xfeed_0020
    );
    let reads = endpoint.reads.lock().unwrap();
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, DeviceId::new(0));
    assert_eq!(reads[0].1.offset(), 0x20);
    drop(reads);

    drop(runtime);
    assert_eq!(
        binding.read_bar(bar.address(), AccessWidth::Dword),
        Err(DeviceError::NotFound)
    );
}

#[test]
fn failed_bundle_registration_invalidates_the_provisional_route() {
    let endpoint = RecordingEndpoint::shared();
    let (graph, root_slot, binding_slot) = resolved_graph(endpoint, true);
    let mut builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
    let mut nodes = graph.nodes();
    builder
        .build_graph_node(nodes.next().unwrap(), graph.resource_plan())
        .unwrap();
    assert!(
        builder
            .build_graph_node(nodes.next().unwrap(), graph.resource_plan())
            .is_err()
    );
    let root = root_slot.lock().unwrap().clone().unwrap();
    let binding = binding_slot.lock().unwrap().clone().unwrap();
    let (function, bar) = endpoint_bar(&graph, "endpoint");
    enable_mse(&root, function);
    assert_eq!(
        binding.read_bar(bar.address(), AccessWidth::Dword),
        Err(DeviceError::NotFound)
    );
}

#[test]
fn host_bundles_without_exactly_one_matching_root_service_fail() {
    // Missing service entirely.
    let message = expect_build_error(
        &resolved_graph_with_modes(
            RecordingEndpoint::shared(),
            false,
            HostMode::NoService,
            false,
        )
        .0,
    );
    assert!(message.contains("must publish exactly one PciRootBinding"));

    // Two services for the same key.
    let message = expect_build_error(
        &resolved_graph_with_modes(
            RecordingEndpoint::shared(),
            false,
            HostMode::DuplicateService,
            false,
        )
        .0,
    );
    assert!(message.contains("must publish exactly one PciRootBinding"));

    // Service owned by another host identity.
    let message = expect_build_error(
        &resolved_graph_with_modes(
            RecordingEndpoint::shared(),
            false,
            HostMode::WrongOwner,
            false,
        )
        .0,
    );
    assert!(message.contains("mismatched PCI root"));

    // Service bound to an unrelated topology instance.
    let message = expect_build_error(
        &resolved_graph_with_modes(
            RecordingEndpoint::shared(),
            false,
            HostMode::ForeignTopology,
            false,
        )
        .0,
    );
    assert!(message.contains("mismatched PCI root"));
}

#[test]
fn ordinary_nodes_cannot_publish_a_pci_root() {
    let binding: BindingSlot = Arc::new(Mutex::new(None));
    let plain_model = Arc::new(PlainRootModel {
        binding: binding.clone(),
    });
    let mut graph = DeviceGraphBuilder::new();
    graph
        .register_pci_host(host_provider(
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
        ))
        .unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("plain-root-node"),
            plain_model,
        ))
        .unwrap();
    let resolved = graph.declare().unwrap().resolve(test_pools()).unwrap();

    let message = expect_build_error(&resolved);
    assert!(message.contains("published a PCI root"));
    // The rejected service was published before registration failed; the
    // runtime must have rolled back without keeping any partial state.
    assert!(binding.lock().unwrap().is_some());
}

#[test]
fn endpoint_nodes_must_declare_their_bundled_pci_function() {
    let (graph, ..) =
        resolved_graph_with_modes(RecordingEndpoint::shared(), false, HostMode::Correct, true);
    let message = expect_build_error(&graph);
    assert!(message.contains("did not declare its bundled PCI function"));
}

#[test]
fn non_pci_nodes_cannot_bundle_a_pci_function() {
    let provider = host_provider(Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)));
    let mut graph = DeviceGraphBuilder::new();
    graph.register_pci_host(provider).unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("sneaky"),
            Arc::new(UndeclaredFunctionModel {
                function: RecordingEndpoint::shared(),
            }),
        ))
        .unwrap();
    let resolved = graph.declare().unwrap().resolve(test_pools()).unwrap();

    let message = expect_build_error(&resolved);
    assert!(message.contains("declared a bundled PCI function"));
}

#[test]
fn legacy_register_bundle_rejects_pci_carrying_bundles() {
    // Reach the public legacy path through a runtime built without PCI:
    // a plain host-less topology keeps this focused on the guard itself.
    struct PlainModel;
    impl DeviceModel for PlainModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            Ok(DeviceRequirements::new())
        }
        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }
        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            Ok(DeviceBundle::new())
        }
    }

    let mut graph = DeviceGraphBuilder::new();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("plain"),
            Arc::new(PlainModel),
        ))
        .unwrap();
    let resolved = graph
        .declare()
        .unwrap()
        .resolve(ResourcePools::new())
        .unwrap();
    let mut builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
    let node = resolved.nodes().next().unwrap();
    builder
        .build_graph_node(node, resolved.resource_plan())
        .unwrap();
    let mut runtime = match builder.finish(resolved.resource_plan()) {
        Ok(runtime) => runtime,
        Err(_) => return, // sealed runtimes reject everything anyway
    };

    let mut bundle = DeviceBundle::new();
    bundle
        .add_pci_function(RecordingEndpoint::shared())
        .unwrap();
    let error = runtime.register_bundle(bundle).unwrap_err();
    assert!(error.to_string().contains("require resolved graph-node"));
}

/// Two endpoints behind one host route through their own functions and BAR
/// indexes; dispatch never crosses objects.
/// Read log shared between a tagged endpoint and its assertions.
type SharedReadLog = Arc<Mutex<Vec<(u64, PciBarAccess)>>>;

#[test]
fn two_endpoints_route_through_their_own_functions_and_bar_indexes() {
    struct Tagged {
        tag: u64,
        reads: SharedReadLog,
    }
    impl Tagged {
        fn new(tag: u64) -> (Arc<Self>, SharedReadLog) {
            let reads = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::new(Self {
                    tag,
                    reads: Arc::clone(&reads),
                }),
                reads,
            )
        }
    }
    impl Device for Tagged {
        fn name(&self) -> &str {
            "tagged-pci-endpoint"
        }
        fn resources(&self) -> &[Resource] {
            &[]
        }
        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            Err(DeviceError::NotFound)
        }
        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Ok(())
        }
    }
    impl PciFunction for Tagged {
        fn read_bar(
            &self,
            access: PciBarAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            self.reads.lock().unwrap().push((self.tag, access));
            Ok(self.tag << 16 | access.offset())
        }
        fn write_bar(
            &self,
            _access: PciBarAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Ok(())
        }
    }

    struct TwoBarModel {
        function: Arc<Tagged>,
        indexes: &'static [u8],
    }
    impl DeviceModel for TwoBarModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            let mut requirement = PciFunctionRequirement::new(
                host_key(),
                PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0xff, 0, 0)),
            );
            for &index in self.indexes {
                requirement = requirement.with_bar(PciMemoryBar::new(
                    PciBarIndex::new(index).unwrap(),
                    0x1_0000,
                )?)?;
            }
            DeviceRequirements::new().with_pci_function(requirement)
        }
        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::None
        }
        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            let mut bundle = DeviceBundle::new();
            bundle.add_pci_function(self.function.clone())?;
            Ok(bundle)
        }
    }

    let (alpha, alpha_reads) = Tagged::new(1);
    let (beta, beta_reads) = Tagged::new(2);

    let mut graph = DeviceGraphBuilder::new();
    let root_slot: RootSlot = Arc::new(Mutex::new(None));
    let binding_slot: BindingSlot = Arc::new(Mutex::new(None));
    graph
        .register_pci_host(host_provider(
            Arc::clone(&root_slot),
            Arc::clone(&binding_slot),
        ))
        .unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("alpha"),
            Arc::new(TwoBarModel {
                function: Arc::clone(&alpha),
                indexes: &[0],
            }),
        ))
        .unwrap();
    graph
        .add(DeviceNodeSpec::virtual_device(
            id("beta"),
            Arc::new(TwoBarModel {
                function: Arc::clone(&beta),
                indexes: &[2, 4],
            }),
        ))
        .unwrap();
    let resolved = graph.declare().unwrap().resolve(test_pools()).unwrap();
    let runtime = build_runtime(&resolved);
    let binding = runtime
        .services()
        .all::<PciRootBindingKey>()
        .into_iter()
        .next()
        .unwrap();

    let topology = resolved.pci_topology(&host_key()).unwrap();
    let alpha_fn = topology.function(&id("alpha")).unwrap();
    let beta_fn = topology.function(&id("beta")).unwrap();
    assert_ne!(alpha_fn.bdf(), beta_fn.bdf());

    let alpha_bar = alpha_fn.bar(PciBarIndex::new(0).unwrap()).unwrap();
    let beta_bar2 = beta_fn.bar(PciBarIndex::new(2).unwrap()).unwrap();
    let beta_bar4 = beta_fn.bar(PciBarIndex::new(4).unwrap()).unwrap();
    let root = root_slot.lock().unwrap().clone().unwrap();
    for function in [alpha_fn, beta_fn] {
        root.write_config(
            function.bdf(),
            ConfigOffset::new(4).unwrap(),
            AccessWidth::Word,
            2,
        )
        .unwrap();
    }

    assert_eq!(
        binding
            .read_bar(alpha_bar.address() + 4, AccessWidth::Dword)
            .unwrap()
            >> 16,
        1
    );
    assert_eq!(
        binding
            .read_bar(beta_bar2.address() + 8, AccessWidth::Dword)
            .unwrap()
            >> 16,
        2
    );
    assert_eq!(
        binding.write_bar(beta_bar4.address(), AccessWidth::Byte, 0),
        Ok(())
    );

    assert_eq!(alpha_reads.lock().unwrap().len(), 1);
    assert_eq!(
        alpha_reads.lock().unwrap()[0].1.bar(),
        PciBarIndex::new(0).unwrap()
    );
    // The BAR4 access above is a write; Tagged only records reads.
    assert_eq!(beta_reads.lock().unwrap().len(), 1);
    assert_eq!(
        beta_reads.lock().unwrap()[0].1.bar(),
        PciBarIndex::new(2).unwrap()
    );
    drop(runtime);
    assert_eq!(
        binding.read_bar(alpha_bar.address(), AccessWidth::Byte),
        Err(DeviceError::NotFound)
    );
}

/// Concurrent dispatch against a dropping lease must yield only defined
/// outcomes and never cross object boundaries.
#[test]
fn dispatch_races_lease_drop_without_cross_object_results() {
    let endpoint = RecordingEndpoint::shared();
    let (graph, root_slot, binding_slot) = resolved_graph(endpoint.clone(), false);
    let runtime = build_runtime(&graph);
    let root = root_slot.lock().unwrap().clone().unwrap();
    let binding = binding_slot.lock().unwrap().clone().unwrap();
    let (function, bar) = endpoint_bar(&graph, "endpoint");
    enable_mse(&root, function);

    let reader_binding = Arc::clone(&binding);
    let reader = std::thread::spawn(move || {
        let mut results = Vec::new();
        for _ in 0..4096 {
            results.push(reader_binding.read_bar(bar.address(), AccessWidth::Dword));
        }
        results
    });

    drop(runtime);
    let results = reader.join().unwrap();
    let unexpected: Vec<_> = results
        .into_iter()
        .filter(|result| {
            !matches!(
                result,
                Ok(0xfeed_0000)
                    | Err(DeviceError::NotFound)
                    | Err(DeviceError::InvalidState { .. })
            )
        })
        .collect();
    assert!(unexpected.is_empty(), "unexpected results: {unexpected:?}");
    assert_eq!(
        binding.read_bar(bar.address(), AccessWidth::Dword),
        Err(DeviceError::NotFound)
    );
}
