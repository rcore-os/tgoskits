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
    AxVmDeviceConfig, AxVmDevices, ControllerInputId, ControllerRegistration, ControllerRole,
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, InterruptControllerId, InterruptTopology,
    InterruptTriggerMode, IrqError, IrqLine, IrqResult, MmioDeviceAdapter, WiredInterruptInputs,
    WiredIrqInput, WiredIrqRequest, WiredIrqSink,
};
use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceResult};
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, VMInterruptMode,
};

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

struct IrqMmioFactory;

impl DeviceFactory for IrqMmioFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioNet
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let Some(end) = config.base_gpa.checked_add(config.length) else {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build IRQ MMIO test device",
                detail: "device address range overflows".into(),
            });
        };
        let line = context.connect_irq(WiredIrqRequest::new(
            ControllerInputId::new(config.irq_id),
            InterruptTriggerMode::EdgeTriggered,
        ))?;
        Ok(
            DeviceRegistration::Device(MmioDeviceAdapter::from_arc(Arc::new(IrqMmioDevice {
                range: GuestPhysAddrRange::new(config.base_gpa.into(), end.into()),
                line,
            })))
            .into(),
        )
    }
}

fn irq_device_config(base_gpa: usize, irq_id: usize) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: String::from("irq-mmio"),
        base_gpa,
        length: 0x1000,
        irq_id,
        emu_type: EmulatedDeviceType::VirtioNet,
        cfg_list: vec![],
    }
}

fn irq_factory_registry() -> DeviceFactoryRegistry {
    let mut factories = DeviceFactoryRegistry::new();
    factories.register(Arc::new(IrqMmioFactory)).unwrap();
    factories
}

fn recording_topology() -> (InterruptTopology, Weak<RecordingIrqSink>) {
    let sink = Arc::new(RecordingIrqSink::default());
    let weak = Arc::downgrade(&sink);
    let topology = InterruptTopology::new(VMInterruptMode::Emulated);
    topology
        .register_controller(
            ControllerRegistration::new(TEST_CONTROLLER, ControllerRole::Default)
                .with_wired_inputs(Arc::new(RecordingControllerInputs { sink })),
        )
        .unwrap();
    (topology, weak)
}

#[test]
fn test_no_irq_topology_rejects_controller_and_line_connection() {
    let topology = InterruptTopology::new(VMInterruptMode::NoIrq);
    let sink = Arc::new(RecordingIrqSink::default());
    assert!(matches!(
        topology.register_controller(
            ControllerRegistration::new(TEST_CONTROLLER, ControllerRole::Default)
                .with_wired_inputs(Arc::new(RecordingControllerInputs { sink }))
        ),
        Err(DeviceManagerError::Unsupported { .. })
    ));

    let context = DeviceBuildContext::new(&topology);
    assert!(matches!(
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x6_0000, 12)]),
            &irq_factory_registry(),
            &context,
        ),
        Err(DeviceManagerError::Unsupported { .. })
    ));
}

#[test]
fn test_interrupt_topology_preserves_event_order() {
    let (topology, sink) = recording_topology();
    let level = topology
        .connect_irq(WiredIrqRequest::new(
            ControllerInputId::new(13),
            InterruptTriggerMode::LevelTriggered,
        ))
        .unwrap();
    let edge = topology
        .connect_irq(WiredIrqRequest::new(
            ControllerInputId::new(14),
            InterruptTriggerMode::EdgeTriggered,
        ))
        .unwrap();

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
fn test_factory_device_emits_irq_through_connected_input() {
    let (topology, sink) = recording_topology();
    let devices = {
        let context = DeviceBuildContext::new(&topology);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x7_0000, 15)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };

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
    let (topology, sink) = recording_topology();
    let devices = {
        let context = DeviceBuildContext::new(&topology);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x8_0000, 16)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };

    drop(topology);
    assert!(sink.upgrade().is_some());
    drop(devices);
    assert!(sink.upgrade().is_none());
}

#[test]
fn test_equal_input_numbers_are_isolated_between_topologies() {
    let (topology_a, sink_a) = recording_topology();
    let (topology_b, sink_b) = recording_topology();
    let devices_a = {
        let context = DeviceBuildContext::new(&topology_a);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x9_0000, 17)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };
    let devices_b = {
        let context = DeviceBuildContext::new(&topology_b);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0xa_0000, 17)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };

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
