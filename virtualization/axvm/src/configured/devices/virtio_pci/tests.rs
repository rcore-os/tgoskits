use std::{
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use axdevice::{
    ConfigOffset, ControllerRegistration, DeviceBuildContext, DeviceBundle, DeviceGraphBuilder,
    DeviceManagerError, DeviceManagerResult, DeviceModel, DeviceNodeSpec, DeviceRegistration,
    DeviceRequirements, DeviceRuntimeBuilder, PciBarIndex, PciClass, PciEndpointIdentity,
    PciFunctionRequirement, PciHostKey, PciHostProvider, PciIntxPin, PciIntxRequirement,
    PciIntxRouter, PciMemoryBar, PciRootBinding, PciRootBindingKey, ResourcePools, ResourceRequest,
    ResourceSlot, RuntimeAccessPorts,
};
use axdevice_base::{
    AccessWidth, ControllerInputId, DeviceContext, DeviceError, DeviceId, DmaGrant, GuestPhysAddr,
    InterruptControllerId, InterruptEndpoint, InterruptSharing, InterruptTrigger, IrqError,
    IrqResult, VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvirtio_common::pci::VirtioPciCapabilitySet;

use super::*;

const HOST_KEY: &str = "x86-q35";
const APERTURE_BASE: u64 = 0xc000_0000;
const APERTURE_SIZE: u64 = 0x10_0000;
const BAR_SIZE: u64 = 0x1000;
const INTX_CONTROLLER: InterruptControllerId = InterruptControllerId::new(0);

struct TestPause {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    armed: AtomicBool,
}

impl TestPause {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::new(Barrier::new(2)),
            release: Arc::new(Barrier::new(2)),
            armed: AtomicBool::new(true),
        })
    }

    fn wait(&self) {
        if self
            .armed
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.entered.wait();
            self.release.wait();
        }
    }
}

fn host_key() -> PciHostKey {
    PciHostKey::new(HOST_KEY).expect("static PCI host key is valid")
}

fn node(value: &str) -> axdevice::DeviceNodeId {
    axdevice::DeviceNodeId::new(value).expect("static node id is valid")
}

fn slot(value: &str) -> ResourceSlot {
    ResourceSlot::new(value).expect("static resource slot is valid")
}

struct TestIrqSink {
    fail_assert: AtomicBool,
    fail_deassert: AtomicBool,
    assert_calls: AtomicUsize,
    deassert_calls: AtomicUsize,
    assert_pause: Mutex<Option<Arc<TestPause>>>,
}

impl TestIrqSink {
    fn new() -> Self {
        Self {
            fail_assert: AtomicBool::new(false),
            fail_deassert: AtomicBool::new(false),
            assert_calls: AtomicUsize::new(0),
            deassert_calls: AtomicUsize::new(0),
            assert_pause: Mutex::new(None),
        }
    }

    fn pause_next_assert(&self, pause: Arc<TestPause>) {
        *self
            .assert_pause
            .lock()
            .expect("test IRQ pause lock should not be poisoned") = Some(pause);
    }

    fn failure(input: ControllerInputId, asserted: bool) -> IrqError {
        IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: INTX_CONTROLLER,
                input,
            },
            operation: if asserted {
                "assert test VirtIO IRQ"
            } else {
                "deassert test VirtIO IRQ"
            },
            detail: "injected test IRQ sink failure".into(),
        }
    }
}

impl WiredIrqSink for TestIrqSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        if asserted {
            let pause = self
                .assert_pause
                .lock()
                .expect("test IRQ pause lock should not be poisoned")
                .take();
            if let Some(pause) = pause {
                pause.wait();
            }
            self.assert_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_assert.load(Ordering::Relaxed) {
                return Err(Self::failure(input, true));
            }
        } else {
            self.deassert_calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_deassert.load(Ordering::Relaxed) {
                return Err(Self::failure(input, false));
            }
        }
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        Ok(())
    }
}

struct TestInterruptController {
    sink: Arc<TestIrqSink>,
}

impl VirtualInterruptController for TestInterruptController {
    fn id(&self) -> InterruptControllerId {
        INTX_CONTROLLER
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTrigger,
    ) -> IrqResult<WiredIrqInput> {
        Ok(WiredIrqInput::new(
            self.id(),
            input,
            trigger,
            self.sink.clone(),
        ))
    }
}

struct TestCore {
    fail_notify: bool,
}

impl VirtioDeviceCore for TestCore {
    fn device_type(&self) -> axvirtio_common::VirtioDeviceID {
        axvirtio_common::VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        0
    }

    fn queue_size_max(&self) -> u16 {
        128
    }

    fn device_config_size(&self) -> u32 {
        16
    }

    fn read_device_config(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        Ok(0xfeed_0000 | offset | ((width.size() as u64) << 24))
    }

    fn write_device_config(&self, _offset: u64, _width: AccessWidth, _value: u64) -> DeviceResult {
        Ok(())
    }

    fn notify_queue(
        &self,
        _queue: &mut axvirtio_common::VirtioQueue<axvirtio_common::NoGuestMemoryAccessor>,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> DeviceResult<axvirtio_common::pci::QueueNotifyOutcome> {
        if self.fail_notify {
            return Err(DeviceError::Internal);
        }
        let mut input = [0u8; 4];
        memory
            .read(GuestPhysAddr::from_usize(0x4000), &mut input)
            .map_err(|_| DeviceError::Internal)?;
        memory
            .write(GuestPhysAddr::from_usize(0x5000), &input)
            .map_err(|_| DeviceError::Internal)?;
        Ok(axvirtio_common::pci::QueueNotifyOutcome::Completed { notify: true })
    }
}

struct TestEndpointContext {
    device_id: DeviceId,
    reads: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
    pause: Option<(Arc<Barrier>, Arc<Barrier>)>,
}

impl TestEndpointContext {
    fn new() -> Self {
        Self {
            device_id: DeviceId::new(0),
            reads: Arc::new(AtomicUsize::new(0)),
            writes: Arc::new(AtomicUsize::new(0)),
            pause: None,
        }
    }

    fn paused(mut self, entered: Arc<Barrier>, release: Arc<Barrier>) -> Self {
        self.pause = Some((entered, release));
        self
    }

    fn nested(&self, device_id: DeviceId) -> Self {
        Self {
            device_id,
            reads: Arc::clone(&self.reads),
            writes: Arc::clone(&self.writes),
            pause: self.pause.clone(),
        }
    }
}

impl DeviceContext for TestEndpointContext {
    fn device_id(&self) -> DeviceId {
        self.device_id
    }

    fn with_routed_device(
        &mut self,
        grant: &axdevice_base::RoutedDeviceGrant,
        callback: &mut dyn FnMut(&mut dyn DeviceContext) -> DeviceResult,
    ) -> DeviceResult {
        if let Some((entered, release)) = &self.pause {
            entered.wait();
            release.wait();
        }
        let mut nested = self.nested(grant.device_id());
        callback(&mut nested)
    }

    fn read_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        _addr: GuestPhysAddr,
        data: &mut [u8],
    ) -> DeviceResult {
        self.reads.fetch_add(1, Ordering::Relaxed);
        data.fill(0xa5);
        Ok(())
    }

    fn write_guest_memory(
        &mut self,
        _grant: &DmaGrant,
        _addr: GuestPhysAddr,
        _data: &[u8],
    ) -> DeviceResult {
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

struct HostModel {
    root: Arc<Mutex<Option<Arc<axdevice::PciRootState>>>>,
    sink: Arc<TestIrqSink>,
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

    fn firmware(&self) -> axdevice::DeviceFirmwareSpec {
        axdevice::DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let _ = context.mmio("pci-memory")?;
        let topology =
            context
                .pci_host_topology()
                .cloned()
                .ok_or(DeviceManagerError::InvalidState {
                    operation: "build VirtIO PCI test host",
                    detail: "test host topology was not resolved".into(),
                })?;
        let root = Arc::new(axdevice::PciRootState::new(topology));
        *self
            .root
            .lock()
            .expect("test root lock should not be poisoned") = Some(root.clone());
        let binding = Arc::new(PciRootBinding::new(node("pci-host"), root));
        let bundle = DeviceBundle::from_registration(DeviceRegistration::InterruptController(
            ControllerRegistration::new(
                INTX_CONTROLLER,
                Arc::new(TestInterruptController {
                    sink: Arc::clone(&self.sink),
                }),
            ),
        ));
        bundle.with_service::<PciRootBindingKey>(binding)
    }
}

struct EndpointModel {
    fail_notify: bool,
    command_revision_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl DeviceModel for EndpointModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let capabilities = virtio_capabilities(&VirtioPciCapabilitySet::new(16))
            .map_err(DeviceManagerError::Pci)?;
        let mut requirement = PciFunctionRequirement::new(
            host_key(),
            PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(0x01, 0x80, 0x00))
                .with_revision(1)
                .with_subsystem_ids(0x1af4, 0x1042),
        )
        .with_bar(PciMemoryBar::new(PciBarIndex::new(0)?, BAR_SIZE)?)?
        .with_intx(PciIntxRequirement::new(PciIntxPin::A, slot("virtio-intx")))?;
        for capability in capabilities {
            requirement = requirement.with_capability(capability);
        }
        DeviceRequirements::new().with_pci_function(requirement)
    }

    fn firmware(&self) -> axdevice::DeviceFirmwareSpec {
        axdevice::DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let grant = DmaGrant::new();
        let irq_line = context.irq("virtio-intx")?;
        let function = Arc::new(
            VirtioPciFunction::try_new(
                TestCore {
                    fail_notify: self.fail_notify,
                },
                grant.clone(),
                irq_line,
            )
            .map_err(DeviceManagerError::Device)?,
        );
        if let Some(hook) = self.command_revision_hook.clone() {
            function.set_command_revision_hook(move || hook());
        }
        let mut bundle = DeviceBundle::new();
        let device_index = bundle.add_pci_function(function)?;
        bundle.grant_guest_memory_to_device(device_index, grant);
        Ok(bundle)
    }
}

fn build_bound_endpoint() -> (
    Arc<axdevice::PciRootState>,
    Arc<PciRootBinding>,
    axdevice::PciBdf,
    axdevice::DeviceRuntime,
) {
    let (root, binding, bdf, runtime, _) = build_bound_endpoint_with_options(false);
    (root, binding, bdf, runtime)
}

fn build_bound_endpoint_with_options(
    fail_notify: bool,
) -> (
    Arc<axdevice::PciRootState>,
    Arc<PciRootBinding>,
    axdevice::PciBdf,
    axdevice::DeviceRuntime,
    Arc<TestIrqSink>,
) {
    build_bound_endpoint_with_command_hook(fail_notify, None)
}

fn build_bound_endpoint_with_command_hook(
    fail_notify: bool,
    command_revision_hook: Option<Arc<dyn Fn() + Send + Sync>>,
) -> (
    Arc<axdevice::PciRootState>,
    Arc<PciRootBinding>,
    axdevice::PciBdf,
    axdevice::DeviceRuntime,
    Arc<TestIrqSink>,
) {
    let root_slot = Arc::new(Mutex::new(None));
    let sink = Arc::new(TestIrqSink::new());
    let provider = PciHostProvider::new(
        host_key(),
        DeviceNodeSpec::virtual_device(
            node("pci-host"),
            Arc::new(HostModel {
                root: root_slot.clone(),
                sink: Arc::clone(&sink),
            }),
        ),
        slot("pci-memory"),
    )
    .with_intx_router(PciIntxRouter::new(
        INTX_CONTROLLER,
        [
            ControllerInputId::new(16),
            ControllerInputId::new(17),
            ControllerInputId::new(18),
            ControllerInputId::new(19),
        ],
        [16, 17, 18, 19],
        InterruptTrigger::LevelTriggered,
        InterruptSharing::Shared,
    ));
    let mut builder = DeviceGraphBuilder::new();
    builder.register_pci_host(provider).unwrap();
    builder
        .add(DeviceNodeSpec::virtual_device(
            node("virtio-pci"),
            Arc::new(EndpointModel {
                fail_notify,
                command_revision_hook,
            }),
        ))
        .unwrap();
    let mut pools = ResourcePools::new();
    pools
        .add_auto_mmio(APERTURE_BASE..APERTURE_BASE + APERTURE_SIZE)
        .unwrap();
    pools
        .allow_fixed_controller_inputs(
            INTX_CONTROLLER,
            ControllerInputId::new(16)..ControllerInputId::new(20),
        )
        .unwrap();
    let graph = builder.declare().unwrap().resolve(pools).unwrap();
    let mut runtime_builder = DeviceRuntimeBuilder::new(RuntimeAccessPorts::new());
    for graph_node in graph.nodes() {
        runtime_builder
            .build_graph_node(graph_node, graph.resource_plan())
            .unwrap();
    }
    let runtime = runtime_builder.finish(graph.resource_plan()).unwrap();
    let root = root_slot.lock().unwrap().clone().unwrap();
    let binding = runtime
        .services()
        .all::<PciRootBindingKey>()
        .into_iter()
        .next()
        .unwrap();
    let bdf = graph
        .pci_topology(&host_key())
        .unwrap()
        .function(&node("virtio-pci"))
        .unwrap()
        .bdf();
    (root, binding, bdf, runtime, sink)
}

fn configure_running_endpoint(
    root: &axdevice::PciRootState,
    binding: &PciRootBinding,
    bdf: axdevice::PciBdf,
    bar: u64,
    context: &mut TestEndpointContext,
) {
    root.write_config(bdf, ConfigOffset::new(4).unwrap(), AccessWidth::Word, 6)
        .expect("VirtIO memory and bus mastering should enable");
    for status in [1, 3, 0x0b, 0x0f] {
        binding
            .write_bar_with_context(bar + 0x14, AccessWidth::Byte, status, context)
            .expect("VirtIO driver status should advance in order");
    }
    for (offset, width, value) in [
        (0x20, AccessWidth::Qword, 0x1000),
        (0x28, AccessWidth::Qword, 0x2000),
        (0x30, AccessWidth::Qword, 0x3000),
    ] {
        binding
            .write_bar_with_context(bar + offset, width, value, context)
            .expect("VirtIO transport configuration should succeed");
    }
    binding
        .write_bar_with_context(bar + 0x1c, AccessWidth::Word, 1, context)
        .expect("VirtIO queue should enable");
}

mod config;
mod integration;
