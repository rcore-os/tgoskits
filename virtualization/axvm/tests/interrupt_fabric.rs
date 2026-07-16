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
    AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceBundle, DeviceFactory,
    DeviceFactoryRegistry, DeviceManagerError, DeviceManagerResult, DeviceRegistration,
    IrqResolver, MmioDeviceAdapter,
};
use axdevice_base::{
    AccessWidth, BaseDeviceOps, DeviceResult, InterruptTriggerMode, IrqError, IrqLine, IrqLineId,
    IrqResult, IrqSink,
};
use axvm::{AxVmError, InterruptFabric};
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, VMInterruptMode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IrqEvent {
    SetLevel(IrqLineId, bool),
    Pulse(IrqLineId),
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

impl IrqSink for RecordingIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> IrqResult {
        self.events
            .lock()
            .unwrap()
            .push(IrqEvent::SetLevel(line, asserted));
        Ok(())
    }

    fn pulse(&self, line: IrqLineId) -> IrqResult {
        self.events.lock().unwrap().push(IrqEvent::Pulse(line));
        Ok(())
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
        let line = context.resolve_irq(config.irq_id, InterruptTriggerMode::EdgeTriggered)?;
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

fn recording_fabric(mode: VMInterruptMode) -> (InterruptFabric, Weak<RecordingIrqSink>) {
    let sink = Arc::new(RecordingIrqSink::default());
    let weak = Arc::downgrade(&sink);
    (InterruptFabric::with_sink(mode, sink).unwrap(), weak)
}

#[test]
fn test_no_irq_fabric_rejects_backend_and_line_resolution() {
    let sink = Arc::new(RecordingIrqSink::default());
    assert!(matches!(
        InterruptFabric::with_sink(VMInterruptMode::NoIrq, sink),
        Err(AxVmError::InvalidInput { .. })
    ));

    let fabric = InterruptFabric::new(VMInterruptMode::NoIrq);
    let context = DeviceBuildContext::new(&fabric);
    assert!(matches!(
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x6_0000, 12)]),
            &irq_factory_registry(),
            &context,
        )
        .err(),
        Some(DeviceManagerError::Irq(IrqError::InvalidLine { .. }))
    ));
}

#[test]
fn test_interrupt_fabric_preserves_event_order() {
    let sink = Arc::new(RecordingIrqSink::default());
    let fabric = InterruptFabric::with_sink(VMInterruptMode::Emulated, sink.clone()).unwrap();
    let level = fabric
        .resolve_irq(13, InterruptTriggerMode::LevelTriggered)
        .unwrap();
    let edge = fabric
        .resolve_irq(14, InterruptTriggerMode::EdgeTriggered)
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
        sink.events(),
        vec![
            IrqEvent::SetLevel(IrqLineId(13), true),
            IrqEvent::SetLevel(IrqLineId(13), false),
            IrqEvent::Pulse(IrqLineId(14)),
        ]
    );
}

#[test]
fn test_interrupt_fabric_can_signal_backend_directly() {
    let sink = Arc::new(RecordingIrqSink::default());
    let fabric = InterruptFabric::with_sink(VMInterruptMode::Emulated, sink.clone()).unwrap();

    fabric.set_level(21, true).unwrap();
    fabric.set_level(21, false).unwrap();
    fabric.pulse(22).unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(IrqLineId(21), true),
            IrqEvent::SetLevel(IrqLineId(21), false),
            IrqEvent::Pulse(IrqLineId(22)),
        ]
    );
}

#[test]
fn test_factory_device_emits_irq_through_interrupt_fabric() {
    let (fabric, sink) = recording_fabric(VMInterruptMode::Emulated);
    let devices = {
        let context = DeviceBuildContext::new(&fabric);
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
        vec![IrqEvent::Pulse(IrqLineId(15))]
    );
}

#[test]
fn test_dropping_devices_and_fabric_releases_irq_backend() {
    let (fabric, sink) = recording_fabric(VMInterruptMode::Emulated);
    let devices = {
        let context = DeviceBuildContext::new(&fabric);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x8_0000, 16)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };

    drop(fabric);
    assert!(sink.upgrade().is_some());
    drop(devices);
    assert!(sink.upgrade().is_none());
}

#[test]
fn test_equal_irq_numbers_are_isolated_between_fabrics() {
    let (fabric_a, sink_a) = recording_fabric(VMInterruptMode::Emulated);
    let (fabric_b, sink_b) = recording_fabric(VMInterruptMode::Emulated);
    let devices_a = {
        let context = DeviceBuildContext::new(&fabric_a);
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![irq_device_config(0x9_0000, 17)]),
            &irq_factory_registry(),
            &context,
        )
        .unwrap()
    };
    let devices_b = {
        let context = DeviceBuildContext::new(&fabric_b);
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
        vec![IrqEvent::Pulse(IrqLineId(17))]
    );
    assert!(sink_b.upgrade().unwrap().events().is_empty());
    assert_eq!(devices_b.devices().count(), 1);
}
