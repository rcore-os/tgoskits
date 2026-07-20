// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use axdevice::{
    AxVmDevices, ControllerCascade, ControllerInputId, ControllerRegistration, ControllerRole,
    Device, DeviceBundle, DeviceManagerError, DeviceManagerResult, DeviceRegistration,
    GuestInterruptId, InterruptControllerId, InterruptControllerOutput, InterruptTopology,
    InterruptTriggerMode, IrqLine, IrqResult, MessageInterruptInputs, MessageInterruptSink,
    MsiDeviceId, MsiEndpoint, MsiEventId, MsiMessage, VcpuInterruptAffinity, VcpuInterruptBinding,
    VcpuInterruptController, VcpuInterruptDeactivation, VcpuInterruptId, VcpuInterruptPort,
    VcpuInterruptWake, WiredInterruptInputs, WiredIrqInput, WiredIrqRequest, WiredIrqSink,
};
use axdevice_base::{BusAccess, BusResponse, DeviceError, InterruptSharing, Resource};

const ROOT: InterruptControllerId = InterruptControllerId::new(1);
const CHILD: InterruptControllerId = InterruptControllerId::new(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WiredEvent {
    Level(ControllerInputId, bool),
    Edge(ControllerInputId),
}

#[derive(Default)]
struct RecordingWiredSink {
    events: Mutex<Vec<WiredEvent>>,
}

impl RecordingWiredSink {
    fn events(&self) -> Vec<WiredEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl WiredIrqSink for RecordingWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.events
            .lock()
            .unwrap()
            .push(WiredEvent::Level(input, asserted));
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.events.lock().unwrap().push(WiredEvent::Edge(input));
        Ok(())
    }
}

struct MockWiredInputs {
    controller: InterruptControllerId,
    input_count: usize,
    sink: Arc<RecordingWiredSink>,
    opens: Mutex<Vec<ControllerInputId>>,
}

impl MockWiredInputs {
    fn new(
        controller: InterruptControllerId,
        input_count: usize,
        sink: Arc<RecordingWiredSink>,
    ) -> Self {
        Self {
            controller,
            input_count,
            sink,
            opens: Mutex::new(Vec::new()),
        }
    }

    fn opens(&self) -> Vec<ControllerInputId> {
        self.opens.lock().unwrap().clone()
    }
}

impl WiredInterruptInputs for MockWiredInputs {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() >= self.input_count {
            return Err(axdevice::IrqError::InvalidInput {
                endpoint: axdevice::InterruptEndpoint::Wired {
                    controller: self.controller,
                    input,
                },
                operation: "open controller input",
                detail: "input is outside the implemented range".into(),
            });
        }
        self.opens.lock().unwrap().push(input);
        Ok(WiredIrqInput::new(
            self.controller,
            input,
            trigger,
            self.sink.clone(),
        ))
    }
}

#[derive(Default)]
struct RecordingMessageSink {
    messages: Mutex<Vec<MsiMessage>>,
}

impl MessageInterruptSink for RecordingMessageSink {
    fn signal(&self, message: MsiMessage) -> IrqResult {
        self.messages.lock().unwrap().push(message);
        Ok(())
    }
}

struct MockMessageInputs {
    controller: InterruptControllerId,
    sink: Arc<RecordingMessageSink>,
}

impl MessageInterruptInputs for MockMessageInputs {
    fn connect(&self, device: MsiDeviceId, event: MsiEventId) -> IrqResult<MsiEndpoint> {
        Ok(MsiEndpoint::new(
            self.controller,
            MsiMessage::new(device, event),
            self.sink.clone(),
        ))
    }
}

#[derive(Default)]
struct CapturingOutput {
    line: Mutex<Option<IrqLine>>,
}

impl InterruptControllerOutput for CapturingOutput {
    fn connect_output(&self, line: IrqLine) -> IrqResult {
        *self.line.lock().unwrap() = Some(line);
        Ok(())
    }

    fn disconnect_output(&self) -> IrqResult {
        *self.line.lock().unwrap() = None;
        Ok(())
    }
}

#[derive(Default)]
struct RecordingWake {
    calls: Mutex<usize>,
}

impl VcpuInterruptWake for RecordingWake {
    fn wake(&self) -> DeviceManagerResult {
        *self.calls.lock().unwrap() += 1;
        Ok(())
    }
}

struct RecordingBinding {
    vcpu: VcpuInterruptId,
    events: Arc<Mutex<Vec<(&'static str, VcpuInterruptId)>>>,
}

impl VcpuInterruptBinding for RecordingBinding {
    fn load(&self) -> DeviceManagerResult {
        self.events.lock().unwrap().push(("load", self.vcpu));
        Ok(())
    }

    fn save(&self) -> DeviceManagerResult {
        self.events.lock().unwrap().push(("save", self.vcpu));
        Ok(())
    }

    fn synchronize(&self) -> DeviceManagerResult {
        self.events.lock().unwrap().push(("synchronize", self.vcpu));
        Ok(())
    }
}

#[derive(Default)]
struct RecordingVcpuController {
    attached: Mutex<Vec<(VcpuInterruptId, VcpuInterruptAffinity)>>,
    events: Arc<Mutex<Vec<(&'static str, VcpuInterruptId)>>>,
    deactivations: Mutex<Vec<(VcpuInterruptId, GuestInterruptId)>>,
}

impl VcpuInterruptDeactivation for RecordingVcpuController {
    fn deactivate(&self, vcpu: VcpuInterruptId, intid: GuestInterruptId) -> DeviceManagerResult {
        self.deactivations.lock().unwrap().push((vcpu, intid));
        Ok(())
    }
}

impl VcpuInterruptController for RecordingVcpuController {
    fn attach_vcpu(
        &self,
        port: VcpuInterruptPort,
    ) -> DeviceManagerResult<Arc<dyn VcpuInterruptBinding>> {
        self.attached
            .lock()
            .unwrap()
            .push((port.id(), port.affinity()));
        Ok(Arc::new(RecordingBinding {
            vcpu: port.id(),
            events: self.events.clone(),
        }))
    }
}

fn wired_registration(
    id: InterruptControllerId,
    role: ControllerRole,
) -> (
    ControllerRegistration,
    Arc<MockWiredInputs>,
    Arc<RecordingWiredSink>,
) {
    let sink = Arc::new(RecordingWiredSink::default());
    let inputs = Arc::new(MockWiredInputs::new(id, 64, sink.clone()));
    (
        ControllerRegistration::new(id, role).with_wired_inputs(inputs.clone()),
        inputs,
        sink,
    )
}

struct StaticMmioDevice {
    name: &'static str,
    resources: [Resource; 1],
}

impl StaticMmioDevice {
    fn new(name: &'static str, base: u64, size: u64) -> Self {
        Self {
            name,
            resources: [Resource::MmioRange { base, size }],
        }
    }
}

impl Device for StaticMmioDevice {
    fn name(&self) -> &str {
        self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
        Err(DeviceError::NotFound)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test]
fn resolves_default_and_explicit_controllers_and_caches_inputs() {
    let (topology, authority) = InterruptTopology::new();
    let (root, inputs, sink) = wired_registration(ROOT, ControllerRole::Default);
    topology.register_controller(root).unwrap();

    let first_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::new(
                ControllerInputId::new(33),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Shared,
            ),
        )
        .unwrap();
    let (first, _first_registration) = topology.connect_irq(first_claim).unwrap().into_parts();
    let second_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::for_controller(
                ROOT,
                ControllerInputId::new(33),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Shared,
            ),
        )
        .unwrap();
    let (second, _second_registration) = topology.connect_irq(second_claim).unwrap().into_parts();

    assert_ne!(first.source(), second.source());
    assert_eq!(inputs.opens(), vec![ControllerInputId::new(33)]);
    first.raise().unwrap();
    second.raise().unwrap();
    first.lower().unwrap();
    assert_eq!(
        sink.events(),
        vec![WiredEvent::Level(ControllerInputId::new(33), true)]
    );
    second.lower().unwrap();
    assert_eq!(
        sink.events(),
        vec![
            WiredEvent::Level(ControllerInputId::new(33), true),
            WiredEvent::Level(ControllerInputId::new(33), false),
        ]
    );
}

#[test]
fn rejects_duplicate_ids_and_default_controllers() {
    let (topology, _authority) = InterruptTopology::new();
    let (root, ..) = wired_registration(ROOT, ControllerRole::Default);
    topology.register_controller(root.clone()).unwrap();
    assert!(matches!(
        topology.register_controller(root),
        Err(DeviceManagerError::ResourceConflict { .. })
    ));

    let (second_default, ..) = wired_registration(CHILD, ControllerRole::Default);
    assert!(matches!(
        topology.register_controller(second_default),
        Err(DeviceManagerError::ResourceConflict { .. })
    ));
}

#[test]
fn rejects_a_planner_claim_issued_for_another_topology() {
    let (first, first_authority) = InterruptTopology::new();
    let (second, second_authority) = InterruptTopology::new();
    let (first_controller, ..) = wired_registration(ROOT, ControllerRole::Default);
    let (second_controller, ..) = wired_registration(ROOT, ControllerRole::Default);
    first.register_controller(first_controller).unwrap();
    second.register_controller(second_controller).unwrap();
    let request = WiredIrqRequest::new(
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        InterruptSharing::Exclusive,
    );
    let foreign_claim = first_authority.claim_wired(&first, request).unwrap();

    assert!(matches!(
        second.connect_irq(foreign_claim),
        Err(DeviceManagerError::InvalidInput { .. })
    ));

    let local_claim = second_authority.claim_wired(&second, request).unwrap();
    let (_line, _registration) = second.connect_irq(local_claim).unwrap().into_parts();
}

#[test]
fn equal_input_numbers_on_distinct_controllers_have_distinct_ownership_keys() {
    let (topology, authority) = InterruptTopology::new();
    let (root, ..) = wired_registration(ROOT, ControllerRole::Default);
    let (child, ..) = wired_registration(CHILD, ControllerRole::Secondary);
    topology.register_controller(root).unwrap();
    topology.register_controller(child).unwrap();
    let input = ControllerInputId::new(33);
    let root_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::for_controller(
                ROOT,
                input,
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        )
        .unwrap();
    let child_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::for_controller(
                CHILD,
                input,
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        )
        .unwrap();

    let (_root_line, _root_registration) = topology.connect_irq(root_claim).unwrap().into_parts();
    let (_child_line, _child_registration) =
        topology.connect_irq(child_claim).unwrap().into_parts();

    assert_eq!(topology.active_endpoint_resources().len(), 2);
}

#[test]
fn non_software_source_claim_is_auditable_and_rolls_back_without_opening_an_irq_line() {
    let (topology, authority) = InterruptTopology::new();
    let (root, inputs, _) = wired_registration(ROOT, ControllerRole::Default);
    topology.register_controller(root).unwrap();
    let request = WiredIrqRequest::for_controller(
        ROOT,
        ControllerInputId::new(40),
        InterruptTriggerMode::LevelTriggered,
        InterruptSharing::Exclusive,
    );

    let claim = authority.claim_wired(&topology, request).unwrap();
    let registration = topology.authorize_wired_endpoint(claim).unwrap();
    assert!(inputs.opens().is_empty());
    assert_eq!(topology.active_endpoint_resources().len(), 1);
    assert!(matches!(
        authority.claim_wired(&topology, request),
        Err(DeviceManagerError::ResourceConflict { .. })
    ));

    drop(registration);
    assert!(topology.active_endpoint_resources().is_empty());
    let retry = authority.claim_wired(&topology, request).unwrap();
    drop(topology.authorize_wired_endpoint(retry).unwrap());
    assert!(topology.active_endpoint_resources().is_empty());
}

#[test]
fn connects_controller_cascade_after_validating_parent_graph() {
    let (topology, _authority) = InterruptTopology::new();
    let (root, _, sink) = wired_registration(ROOT, ControllerRole::Default);
    let output = Arc::new(CapturingOutput::default());
    let child = ControllerRegistration::new(CHILD, ControllerRole::Secondary)
        .with_wired_inputs(wired_registration(CHILD, ControllerRole::Secondary).1)
        .with_cascade(ControllerCascade::new(
            WiredIrqRequest::for_controller(
                ROOT,
                ControllerInputId::new(5),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            output.clone(),
        ));
    topology.register_controller(child).unwrap();
    topology.register_controller(root).unwrap();

    topology.finalize(&[]).unwrap();
    let line = output.line.lock().unwrap().clone().unwrap();
    line.raise().unwrap();
    assert_eq!(
        sink.events(),
        vec![WiredEvent::Level(ControllerInputId::new(5), true)]
    );
}

#[test]
fn rejects_missing_parents_and_cascade_cycles() {
    let (missing_parent, _missing_authority) = InterruptTopology::new();
    let child_output = Arc::new(CapturingOutput::default());
    let (child, ..) = wired_registration(CHILD, ControllerRole::Default);
    missing_parent
        .register_controller(child.with_cascade(ControllerCascade::new(
            WiredIrqRequest::for_controller(
                ROOT,
                ControllerInputId::new(0),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            child_output,
        )))
        .unwrap();
    assert!(matches!(
        missing_parent.finalize(&[]),
        Err(DeviceManagerError::ResourceNotFound { .. })
    ));
    assert!(!missing_parent.is_finalized());

    let (cycle, _cycle_authority) = InterruptTopology::new();
    let (root, ..) = wired_registration(ROOT, ControllerRole::Default);
    let (child, ..) = wired_registration(CHILD, ControllerRole::Secondary);
    cycle
        .register_controller(root.with_cascade(ControllerCascade::new(
            WiredIrqRequest::for_controller(
                CHILD,
                ControllerInputId::new(0),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            Arc::new(CapturingOutput::default()),
        )))
        .unwrap();
    cycle
        .register_controller(child.with_cascade(ControllerCascade::new(
            WiredIrqRequest::for_controller(
                ROOT,
                ControllerInputId::new(0),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            Arc::new(CapturingOutput::default()),
        )))
        .unwrap();
    assert!(matches!(
        cycle.finalize(&[]),
        Err(DeviceManagerError::InvalidConfig { .. })
    ));
}

#[test]
fn attaches_vcpu_bindings_and_synchronizes_their_lifecycle() {
    let (topology, _authority) = InterruptTopology::new();
    let controller = Arc::new(RecordingVcpuController::default());
    topology
        .register_controller(
            ControllerRegistration::new(ROOT, ControllerRole::Default)
                .with_vcpu_controller(controller.clone()),
        )
        .unwrap();
    let wake = Arc::new(RecordingWake::default());
    let port = VcpuInterruptPort::new(
        VcpuInterruptId::new(3),
        VcpuInterruptAffinity::new(0x102),
        wake,
    );

    topology.finalize(&[port]).unwrap();
    topology.load_vcpu(VcpuInterruptId::new(3)).unwrap();
    topology.synchronize_vcpu(VcpuInterruptId::new(3)).unwrap();
    topology.save_vcpu(VcpuInterruptId::new(3)).unwrap();

    assert_eq!(
        *controller.attached.lock().unwrap(),
        vec![(VcpuInterruptId::new(3), VcpuInterruptAffinity::new(0x102))]
    );
    assert_eq!(
        *controller.events.lock().unwrap(),
        vec![
            ("load", VcpuInterruptId::new(3)),
            ("synchronize", VcpuInterruptId::new(3)),
            ("save", VcpuInterruptId::new(3)),
        ]
    );
}

#[test]
fn routes_trapped_deactivation_to_the_single_cpu_interface_owner() {
    let (topology, _authority) = InterruptTopology::new();
    let controller = Arc::new(RecordingVcpuController::default());
    topology
        .register_controller(
            ControllerRegistration::new(ROOT, ControllerRole::Default)
                .with_vcpu_controller(controller.clone())
                .with_vcpu_deactivation(controller.clone()),
        )
        .unwrap();
    topology
        .finalize(&[VcpuInterruptPort::new(
            VcpuInterruptId::new(2),
            VcpuInterruptAffinity::new(0x100),
            Arc::new(RecordingWake::default()),
        )])
        .unwrap();

    topology
        .deactivate_vcpu_interrupt(VcpuInterruptId::new(2), GuestInterruptId::new(47))
        .unwrap();

    assert_eq!(
        *controller.deactivations.lock().unwrap(),
        vec![(VcpuInterruptId::new(2), GuestInterruptId::new(47))]
    );
}

#[test]
fn rejects_multiple_cpu_interface_deactivation_owners() {
    let (topology, _authority) = InterruptTopology::new();
    let first = Arc::new(RecordingVcpuController::default());
    let second = Arc::new(RecordingVcpuController::default());
    topology
        .register_controller(
            ControllerRegistration::new(ROOT, ControllerRole::Default)
                .with_vcpu_controller(first.clone())
                .with_vcpu_deactivation(first),
        )
        .unwrap();

    assert!(matches!(
        topology.register_controller(
            ControllerRegistration::new(CHILD, ControllerRole::Secondary)
                .with_vcpu_controller(second.clone())
                .with_vcpu_deactivation(second)
        ),
        Err(DeviceManagerError::ResourceConflict { .. })
    ));
}

#[test]
fn rejects_deactivation_without_a_vcpu_controller() {
    let (topology, _authority) = InterruptTopology::new();
    let controller = Arc::new(RecordingVcpuController::default());

    assert!(matches!(
        topology.register_controller(
            ControllerRegistration::new(ROOT, ControllerRole::Default)
                .with_vcpu_deactivation(controller)
        ),
        Err(DeviceManagerError::InvalidInput { .. })
    ));
}

#[test]
fn connects_and_signals_msi_endpoints() {
    let (topology, authority) = InterruptTopology::new();
    let sink = Arc::new(RecordingMessageSink::default());
    topology
        .register_controller(
            ControllerRegistration::new(ROOT, ControllerRole::Default).with_message_inputs(
                Arc::new(MockMessageInputs {
                    controller: ROOT,
                    sink: sink.clone(),
                }),
            ),
        )
        .unwrap();
    let claim = authority
        .claim_msi(
            &topology,
            axdevice::MsiRequest::new(MsiDeviceId::new(7), MsiEventId::new(11)),
        )
        .unwrap();
    let (endpoint, _registration) = topology.connect_msi(claim).unwrap().into_parts();

    endpoint.signal().unwrap();
    assert_eq!(
        *sink.messages.lock().unwrap(),
        vec![MsiMessage::new(MsiDeviceId::new(7), MsiEventId::new(11))]
    );
}

#[test]
fn rolls_back_controller_when_a_device_resource_conflicts() {
    let (topology, _authority) = InterruptTopology::new();
    let mut devices = AxVmDevices::empty();
    let (controller, ..) = wired_registration(ROOT, ControllerRole::Default);
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::InterruptController(controller.clone()));
    bundle.push(DeviceRegistration::Device(Arc::new(StaticMmioDevice::new(
        "first", 0x1000, 0x1000,
    ))));
    bundle.push(DeviceRegistration::Device(Arc::new(StaticMmioDevice::new(
        "overlap", 0x1800, 0x1000,
    ))));

    assert!(matches!(
        devices.register_bundle_with_topology(bundle, &topology),
        Err(DeviceManagerError::Registry(_))
    ));
    assert_eq!(devices.devices().count(), 0);
    assert_eq!(topology.register_controller(controller), Ok(()));
}

#[test]
fn resets_finalized_topology_after_vm_preparation_fails() {
    let (topology, _authority) = InterruptTopology::new();
    let (root, ..) = wired_registration(ROOT, ControllerRole::Default);
    let output = Arc::new(CapturingOutput::default());
    let (child, ..) = wired_registration(CHILD, ControllerRole::Secondary);
    topology.register_controller(root.clone()).unwrap();
    topology
        .register_controller(child.with_cascade(ControllerCascade::new(
            WiredIrqRequest::for_controller(
                ROOT,
                ControllerInputId::new(9),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
            output.clone(),
        )))
        .unwrap();

    topology.finalize(&[]).unwrap();
    assert!(output.line.lock().unwrap().is_some());

    topology.reset_after_failed_preparation().unwrap();

    assert!(!topology.is_finalized());
    assert!(output.line.lock().unwrap().is_none());
    assert_eq!(topology.register_controller(root), Ok(()));
}

#[test]
fn dropping_a_finalized_topology_disconnects_controller_cascades() {
    let output = Arc::new(CapturingOutput::default());
    {
        let (topology, _authority) = InterruptTopology::new();
        let (root, ..) = wired_registration(ROOT, ControllerRole::Default);
        let (child, ..) = wired_registration(CHILD, ControllerRole::Secondary);
        topology.register_controller(root).unwrap();
        topology
            .register_controller(child.with_cascade(ControllerCascade::new(
                WiredIrqRequest::for_controller(
                    ROOT,
                    ControllerInputId::new(9),
                    InterruptTriggerMode::LevelTriggered,
                    InterruptSharing::Exclusive,
                ),
                output.clone(),
            )))
            .unwrap();

        topology.finalize(&[]).unwrap();
        assert!(output.line.lock().unwrap().is_some());
    }

    assert!(output.line.lock().unwrap().is_none());
}
