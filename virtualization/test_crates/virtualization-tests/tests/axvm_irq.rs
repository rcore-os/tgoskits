// Copyright 2025 The Axvisor Team
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

use std::sync::{Arc, Mutex, Weak};

use axdevice::{
    AxVmDevices, ControllerInputId, ControllerRegistration, ControllerRole, DeviceBuildContext,
    DeviceBundle, DeviceManagerResult, DeviceModelId, DeviceRegistration, DeviceRequirements,
    InterruptControllerId, InterruptPlanAuthority, InterruptSharing, InterruptSourceKind,
    InterruptTopology, InterruptTriggerMode, IrqError, IrqLine, IrqResult, MmioDeviceAdapter,
    ResolvedDeviceResources, ResourceSlot, VirtualDeviceModel, VirtualDeviceModelRegistry,
    WiredInterruptInputs, WiredIrqInput, WiredIrqRequest, WiredIrqSink,
};
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult};
use axvm_types::{EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptDelivery};

const TEST_CONTROLLER: InterruptControllerId = InterruptControllerId::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IrqEvent {
    SetLevel(ControllerInputId, bool),
    Pulse(ControllerInputId),
}

#[derive(Default)]
struct RecordingIrqSink {
    events: Mutex<Vec<IrqEvent>>,
}

impl RecordingIrqSink {
    fn events(&self) -> Vec<IrqEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl WiredIrqSink for RecordingIrqSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.events
            .lock()
            .unwrap()
            .push(IrqEvent::SetLevel(input, asserted));
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.events.lock().unwrap().push(IrqEvent::Pulse(input));
        Ok(())
    }
}

struct RecordingControllerInputs {
    sink: Arc<RecordingIrqSink>,
}

impl WiredInterruptInputs for RecordingControllerInputs {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        Ok(WiredIrqInput::new(
            TEST_CONTROLLER,
            input,
            trigger,
            self.sink.clone(),
        ))
    }
}

struct IrqMmioDevice {
    range: GuestPhysAddrRange,
    line: IrqLine,
}

impl BaseDeviceOps<GuestPhysAddrRange> for IrqMmioDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioNet
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> DeviceResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> DeviceResult {
        self.line
            .pulse()
            .map_err(|error| axdevice_base::DeviceError::Backend {
                operation: "pulse test device IRQ",
                detail: error.to_string(),
            })
    }
}

struct IrqMmioModel;

impl VirtualDeviceModel for IrqMmioModel {
    fn model_id(&self) -> DeviceModelId {
        DeviceModelId::new("test-irq-mmio").unwrap()
    }

    fn requirements(
        &self,
        _template: Option<&axdevice::DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(ResourceSlot::new("registers")?, 0x1000, 0x1000)?
            .with_wired_irq(
                ResourceSlot::new("irq")?,
                InterruptTriggerMode::EdgeTriggered,
                InterruptSourceKind::Software,
                InterruptSharing::Exclusive,
            )
    }

    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = resources.mmio(&ResourceSlot::new("registers")?)?;
        let end = base + size;
        let line = context.irq(&ResourceSlot::new("irq")?)?;
        Ok(
            DeviceRegistration::Device(MmioDeviceAdapter::from_arc(Arc::new(IrqMmioDevice {
                range: GuestPhysAddrRange::new((base as usize).into(), (end as usize).into()),
                line,
            })))
            .into(),
        )
    }
}

fn build_irq_device(
    topology: &InterruptTopology,
    authority: &InterruptPlanAuthority,
    base_gpa: usize,
    irq_id: usize,
) -> AxVmDevices {
    let resources = ResolvedDeviceResources::new()
        .with_mmio(
            ResourceSlot::new("registers").unwrap(),
            base_gpa as u64,
            0x1000,
        )
        .unwrap()
        .with_wired_irq(
            ResourceSlot::new("irq").unwrap(),
            ControllerInputId::new(irq_id),
            InterruptTriggerMode::EdgeTriggered,
            InterruptSharing::Exclusive,
        )
        .unwrap();
    let context = DeviceBuildContext::new(topology, authority, &resources);
    let mut models = VirtualDeviceModelRegistry::new();
    models.register(Arc::new(IrqMmioModel)).unwrap();
    let bundle = models
        .build(
            &DeviceModelId::new("test-irq-mmio").unwrap(),
            &resources,
            context,
        )
        .unwrap();
    let mut devices = AxVmDevices::empty();
    devices
        .register_bundle_with_topology(bundle, topology)
        .unwrap();
    devices
}

fn recording_topology() -> (
    InterruptTopology,
    InterruptPlanAuthority,
    Weak<RecordingIrqSink>,
) {
    let sink = Arc::new(RecordingIrqSink::default());
    let weak = Arc::downgrade(&sink);
    let (topology, authority) = InterruptTopology::new(InterruptDelivery::Mediated);
    topology
        .register_controller(
            ControllerRegistration::new(TEST_CONTROLLER, ControllerRole::Default)
                .with_wired_inputs(Arc::new(RecordingControllerInputs { sink })),
        )
        .unwrap();
    (topology, authority, weak)
}

#[test]
fn test_interrupt_topology_preserves_event_order() {
    let (topology, authority, sink) = recording_topology();
    let level_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::new(
                ControllerInputId::new(13),
                InterruptTriggerMode::LevelTriggered,
                InterruptSharing::Exclusive,
            ),
        )
        .unwrap();
    let (level, _level_registration) = topology.connect_irq(level_claim).unwrap().into_parts();
    let edge_claim = authority
        .claim_wired(
            &topology,
            WiredIrqRequest::new(
                ControllerInputId::new(14),
                InterruptTriggerMode::EdgeTriggered,
                InterruptSharing::Exclusive,
            ),
        )
        .unwrap();
    let (edge, _edge_registration) = topology.connect_irq(edge_claim).unwrap().into_parts();

    assert!(matches!(
        level.pulse(),
        Err(IrqError::InvalidTriggerMode { .. })
    ));
    assert!(matches!(
        edge.raise(),
        Err(IrqError::InvalidTriggerMode { .. })
    ));
    level.raise().unwrap();
    level.lower().unwrap();
    edge.pulse().unwrap();

    assert_eq!(
        sink.upgrade().unwrap().events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(13), true),
            IrqEvent::SetLevel(ControllerInputId::new(13), false),
            IrqEvent::Pulse(ControllerInputId::new(14)),
        ]
    );
}

#[test]
fn test_planned_device_emits_irq_through_connected_input() {
    let (topology, authority, sink) = recording_topology();
    let devices = build_irq_device(&topology, &authority, 0x7_0000, 15);

    devices
        .handle_mmio_write(GuestPhysAddr::from(0x7_0000), AccessWidth::Dword, 1)
        .unwrap();

    assert_eq!(
        sink.upgrade().unwrap().events(),
        vec![IrqEvent::Pulse(ControllerInputId::new(15))]
    );
}

#[test]
fn test_dropping_devices_and_topology_releases_irq_backend() {
    let (topology, authority, sink) = recording_topology();
    let devices = build_irq_device(&topology, &authority, 0x8_0000, 16);

    drop(topology);
    assert!(sink.upgrade().is_some());
    drop(devices);
    assert!(sink.upgrade().is_none());
}

#[test]
fn test_equal_input_numbers_are_isolated_between_topologies() {
    let (topology_a, authority_a, sink_a) = recording_topology();
    let (topology_b, authority_b, sink_b) = recording_topology();
    let devices_a = build_irq_device(&topology_a, &authority_a, 0x9_0000, 17);
    let devices_b = build_irq_device(&topology_b, &authority_b, 0xa_0000, 17);

    devices_a
        .handle_mmio_write(GuestPhysAddr::from(0x9_0000), AccessWidth::Dword, 1)
        .unwrap();

    assert_eq!(
        sink_a.upgrade().unwrap().events(),
        vec![IrqEvent::Pulse(ControllerInputId::new(17))]
    );
    assert!(sink_b.upgrade().unwrap().events().is_empty());
    assert_eq!(devices_b.devices().count(), 1);
}
