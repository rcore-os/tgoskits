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

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, DeviceRuntime,
};
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, ControllerInputId, Device, DeviceAccess,
    DeviceError, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError, IrqLine,
    IrqResult, Resource, VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr};

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

struct RecordingInterruptController {
    sink: Arc<RecordingIrqSink>,
    inputs: Mutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
}

impl RecordingInterruptController {
    fn new(sink: Arc<RecordingIrqSink>) -> Self {
        Self {
            sink,
            inputs: Mutex::new(BTreeMap::new()),
        }
    }
}

impl VirtualInterruptController for RecordingInterruptController {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        let mut inputs = self.inputs.lock().unwrap();
        if let Some((registered_trigger, registered)) = inputs.get(&input.value()) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: self.id(),
                        input,
                    },
                    operation: "open test interrupt input",
                    detail: format!(
                        "input {} is already registered as {registered_trigger:?}",
                        input.value()
                    ),
                });
            }
            return Ok(registered.clone());
        }
        let sink: Arc<dyn WiredIrqSink> = self.sink.clone();
        let registered = WiredIrqInput::new(self.id(), input, trigger, sink);
        inputs.insert(input.value(), (trigger, registered.clone()));
        Ok(registered)
    }
}

struct RejectingInterruptController;

impl VirtualInterruptController for RejectingInterruptController {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        _trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        Err(IrqError::Unsupported {
            endpoint: InterruptEndpoint::Wired {
                controller: self.id(),
                input,
            },
            operation: "open test interrupt input",
            detail: "test controller exposes no wired inputs".into(),
        })
    }
}

struct IrqMmioDevice {
    resources: [Resource; 1],
    line: IrqLine,
}

impl Device for IrqMmioDevice {
    fn name(&self) -> &str {
        "irq-mmio"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if access.is_read {
            Ok(BusResponse::Read { value: 0 })
        } else {
            self.line.pulse().map_err(|error| DeviceError::Backend {
                operation: "pulse test device IRQ",
                detail: error.to_string(),
            })?;
            Ok(BusResponse::Write)
        }
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
        let line = context.resolve_irq(config.irq_id, InterruptTriggerMode::EdgeTriggered)?;
        Ok(DeviceRegistration::Device(Arc::new(IrqMmioDevice {
            resources: [Resource::MmioRange {
                base: config.base_gpa as u64,
                size: (end - config.base_gpa) as u64,
            }],
            line,
        }))
        .into())
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

fn recording_controller() -> (Arc<RecordingInterruptController>, Weak<RecordingIrqSink>) {
    let sink = Arc::new(RecordingIrqSink::default());
    let weak = Arc::downgrade(&sink);
    (Arc::new(RecordingInterruptController::new(sink)), weak)
}

#[test]
fn test_controller_rejection_propagates_through_factory_build() {
    let controller = RejectingInterruptController;
    let context = DeviceBuildContext::new(&controller);
    assert!(matches!(
        DeviceRuntime::build_with_factories(
            &[irq_device_config(0x6_0000, 12)],
            &irq_factory_registry(),
            &context,
        )
        .err(),
        Some(DeviceManagerError::Irq(IrqError::Unsupported { .. }))
    ));
}

#[test]
fn test_controller_owned_wired_inputs_preserve_event_order() {
    let sink = Arc::new(RecordingIrqSink::default());
    let controller = RecordingInterruptController::new(sink.clone());
    let level = controller
        .wired_input(
            ControllerInputId::new(13),
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap()
        .connect()
        .unwrap();
    let edge = controller
        .wired_input(
            ControllerInputId::new(14),
            InterruptTriggerMode::EdgeTriggered,
        )
        .unwrap()
        .connect()
        .unwrap();

    assert!(matches!(
        level.pulse(),
        Err(IrqError::InvalidTriggerMode { .. })
    ));
    assert!(matches!(
        edge.assert(),
        Err(IrqError::InvalidTriggerMode { .. })
    ));
    level.assert().unwrap();
    level.deassert().unwrap();
    edge.pulse().unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(13), true),
            IrqEvent::SetLevel(ControllerInputId::new(13), false),
            IrqEvent::Pulse(ControllerInputId::new(14)),
        ]
    );
}

#[test]
fn test_controller_inputs_signal_backend_without_a_parallel_fabric() {
    let sink = Arc::new(RecordingIrqSink::default());
    let controller = RecordingInterruptController::new(sink.clone());
    let level = controller
        .wired_input(
            ControllerInputId::new(21),
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap()
        .connect()
        .unwrap();
    let edge = controller
        .wired_input(
            ControllerInputId::new(22),
            InterruptTriggerMode::EdgeTriggered,
        )
        .unwrap()
        .connect()
        .unwrap();

    level.assert().unwrap();
    level.deassert().unwrap();
    edge.pulse().unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(21), true),
            IrqEvent::SetLevel(ControllerInputId::new(21), false),
            IrqEvent::Pulse(ControllerInputId::new(22)),
        ]
    );
}

#[test]
fn test_factory_device_emits_irq_through_canonical_controller() {
    let (controller, sink) = recording_controller();
    let devices = {
        let context = DeviceBuildContext::new(controller.as_ref());
        DeviceRuntime::build_with_factories(
            &[irq_device_config(0x7_0000, 15)],
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
fn test_dropping_devices_and_controller_releases_irq_backend() {
    let (controller, sink) = recording_controller();
    let devices = {
        let context = DeviceBuildContext::new(controller.as_ref());
        DeviceRuntime::build_with_factories(
            &[irq_device_config(0x8_0000, 16)],
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };

    drop(controller);
    assert!(sink.upgrade().is_some());
    drop(devices);
    assert!(sink.upgrade().is_none());
}

#[test]
fn test_equal_irq_numbers_are_isolated_between_controllers() {
    let (controller_a, sink_a) = recording_controller();
    let (controller_b, sink_b) = recording_controller();
    let devices_a = {
        let context = DeviceBuildContext::new(controller_a.as_ref());
        DeviceRuntime::build_with_factories(
            &[irq_device_config(0x9_0000, 17)],
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };
    let devices_b = {
        let context = DeviceBuildContext::new(controller_b.as_ref());
        DeviceRuntime::build_with_factories(
            &[irq_device_config(0xa_0000, 17)],
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
