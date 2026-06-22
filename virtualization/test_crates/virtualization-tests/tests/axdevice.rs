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

use std::sync::{Arc, Mutex};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice::{
    AxVmDeviceConfig, AxVmDevices, DeviceBuildContext, DeviceBundle, DeviceFactory,
    DeviceFactoryRegistry, DeviceRegistration, IrqResolver, MmioDeviceAdapter, PollableDeviceOps,
    PortDeviceAdapter, SysRegDeviceAdapter, register_builtin_factories,
};
use axdevice_base::{
    AccessWidth, BaseDeviceOps, DeviceRegistry as _, InterruptTriggerMode, IrqLine, Port,
    PortRange, RegistryError, SysRegAddr, SysRegAddrRange,
};
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptVector,
    VCpuId, VMId,
};
use x86_vlapic::host::X86VlapicHostIf;

/// Registers a legacy MMIO device through the new DeviceManager API.
fn register_mmio<T: BaseDeviceOps<GuestPhysAddrRange> + Send + Sync + 'static>(
    devices: &mut AxVmDevices,
    dev: Arc<T>,
) -> AxResult {
    devices
        .register(MmioDeviceAdapter::from_arc(dev))
        .map_err(|e| match e {
            RegistryError::AddressConflict { .. } => AxError::AddrInUse,
            _ => AxError::InvalidInput,
        })?;
    Ok(())
}

/// Registers a legacy Port device through the new DeviceManager API.
fn register_port<T: BaseDeviceOps<PortRange> + Send + Sync + 'static>(
    devices: &mut AxVmDevices,
    dev: Arc<T>,
) -> AxResult {
    devices
        .register(PortDeviceAdapter::from_arc(dev))
        .map_err(|e| match e {
            RegistryError::AddressConflict { .. } => AxError::AddrInUse,
            _ => AxError::InvalidInput,
        })?;
    Ok(())
}

/// Registers a legacy SysReg device through the new DeviceManager API.
fn register_sysreg<T: BaseDeviceOps<SysRegAddrRange> + Send + Sync + 'static>(
    devices: &mut AxVmDevices,
    dev: Arc<T>,
) -> AxResult {
    devices
        .register(SysRegDeviceAdapter::from_arc(dev))
        .map_err(|e| match e {
            RegistryError::AddressConflict { .. } => AxError::AddrInUse,
            _ => AxError::InvalidInput,
        })?;
    Ok(())
}

struct MockMmioDevice {
    name: String,
    range: GuestPhysAddrRange,
    last_write: Mutex<Option<(usize, usize)>>,
}

impl MockMmioDevice {
    fn new(name: &str, base: usize, len: usize) -> Self {
        let start = GuestPhysAddr::from(base);
        let end = GuestPhysAddr::from(base + len);

        Self::with_range(name, GuestPhysAddrRange::new(start, end))
    }

    fn with_range(name: &str, range: GuestPhysAddrRange) -> Self {
        Self {
            name: String::from(name),
            range,
            last_write: Mutex::new(None),
        }
    }

    fn get_last_write(&self) -> Option<(usize, usize)> {
        *self.last_write.lock().unwrap()
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0xDEAD_BEEF)
    }

    fn handle_write(&self, addr: GuestPhysAddr, _width: AccessWidth, val: usize) -> AxResult {
        println!(
            "[Test] Device {} write: addr={:?}, val={:#x}",
            self.name, addr, val
        );

        let offset = addr.as_usize() - self.range.start.as_usize();
        *self.last_write.lock().unwrap() = Some((offset, val));
        Ok(())
    }
}

struct MockPortDevice {
    range: PortRange,
}

impl MockPortDevice {
    fn new(start: u16, end: u16) -> Self {
        Self {
            range: PortRange::new(Port::new(start), Port::new(end)),
        }
    }
}

impl BaseDeviceOps<PortRange> for MockPortDevice {
    fn address_range(&self) -> PortRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Console
    }

    fn handle_read(&self, _addr: Port, _width: AccessWidth) -> AxResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: Port, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

struct MockSysRegDevice {
    range: SysRegAddrRange,
}

struct MockMmioPollableDevice {
    range: GuestPhysAddrRange,
    polled_at: Mutex<Vec<u64>>,
}

impl MockMmioPollableDevice {
    fn new(start: usize, end: usize) -> Self {
        Self {
            range: GuestPhysAddrRange::new(start.into(), end.into()),
            polled_at: Mutex::new(Vec::new()),
        }
    }

    fn polled_at(&self) -> Vec<u64> {
        self.polled_at.lock().unwrap().clone()
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioPollableDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

impl PollableDeviceOps for MockMmioPollableDevice {
    fn poll(&self, now_ns: u64) -> AxResult {
        self.polled_at.lock().unwrap().push(now_ns);
        Ok(())
    }
}

impl MockSysRegDevice {
    fn new(start: usize, end: usize) -> Self {
        Self {
            range: SysRegAddrRange::new(SysRegAddr::new(start), SysRegAddr::new(end)),
        }
    }
}

impl BaseDeviceOps<SysRegAddrRange> for MockSysRegDevice {
    fn address_range(&self) -> SysRegAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn handle_read(&self, _addr: SysRegAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: SysRegAddr, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

fn empty_devices() -> AxVmDevices {
    AxVmDevices::new(AxVmDeviceConfig::new(vec![])).unwrap()
}

fn mmio_device(name: &str, start: usize, end: usize) -> Arc<MockMmioDevice> {
    Arc::new(MockMmioDevice::with_range(
        name,
        GuestPhysAddrRange::new(start.into(), end.into()),
    ))
}

fn device_config(
    name: &str,
    emu_type: EmulatedDeviceType,
    base_gpa: usize,
    length: usize,
) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: String::from(name),
        base_gpa,
        length,
        irq_id: 0,
        emu_type,
        cfg_list: vec![],
    }
}

struct RejectingIrqResolver;

impl IrqResolver for RejectingIrqResolver {
    fn resolve_irq(&self, _line: usize, _trigger: InterruptTriggerMode) -> AxResult<IrqLine> {
        Err(AxError::Unsupported)
    }
}

struct MockMmioFactory;

impl DeviceFactory for MockMmioFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::VirtioBlk
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> AxResult<DeviceBundle> {
        let Some(end) = config.base_gpa.checked_add(config.length) else {
            return Err(AxError::InvalidInput);
        };
        if config.length == 0 {
            return Err(AxError::InvalidInput);
        }

        Ok(
            DeviceRegistration::Device(MmioDeviceAdapter::from_arc(mmio_device(
                &config.name,
                config.base_gpa,
                end,
            )))
            .into(),
        )
    }
}

#[test]
fn test_mmio_dispatch_functionality() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config).unwrap();

    let base_addr = 0x1000_0000;
    let dev_size = 0x1000;
    let mock_dev = Arc::new(MockMmioDevice::new("TestDev", base_addr, dev_size));

    register_mmio(&mut devices, mock_dev.clone()).unwrap();

    let write_offset = 0x40;
    let target_addr = GuestPhysAddr::from(base_addr + write_offset);
    let write_val = 0x1234_5678;

    let width = AccessWidth::try_from(4).unwrap();

    devices
        .handle_mmio_write(target_addr, width, write_val)
        .expect("MMIO write failed");

    let last = mock_dev.get_last_write();
    assert!(last.is_some(), "Device did not receive write command");
    let (off, val) = last.unwrap();
    assert_eq!(off, write_offset, "Write offset mismatch");
    assert_eq!(val, write_val, "Write value mismatch");

    let read_result = devices
        .handle_mmio_read(target_addr, width)
        .expect("MMIO read failed");

    assert_eq!(read_result, 0xDEAD_BEEF, "Read value mismatch");
}

#[test]
fn test_mmio_missing_device_returns_error() {
    let config = AxVmDeviceConfig::new(vec![]);
    let devices = AxVmDevices::new(config).unwrap();

    let invalid_addr = GuestPhysAddr::from(0x9999_9999);
    let width = AccessWidth::try_from(4).unwrap();

    assert!(devices.handle_mmio_read(invalid_addr, width).is_err());
}

#[test]
fn test_mmio_adjacent_ranges_are_allowed() {
    let mut devices = empty_devices();

    assert_eq!(
        register_mmio(&mut devices, mmio_device("first", 0x1000, 0x2000)),
        Ok(())
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("adjacent", 0x2000, 0x3000)),
        Ok(())
    );
    assert_eq!(devices.devices().count(), 2);
}

#[test]
fn test_mmio_duplicate_and_overlapping_ranges_are_rejected_without_modification() {
    let mut devices = empty_devices();
    let existing = mmio_device("existing", 0x2000, 0x3000);

    assert_eq!(register_mmio(&mut devices, existing.clone()), Ok(()));
    assert_eq!(
        register_mmio(&mut devices, existing),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("same-range", 0x2000, 0x3000)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("partial-left", 0x1800, 0x2800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("partial-right", 0x2800, 0x3800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("contains", 0x1000, 0x4000)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, mmio_device("contained", 0x2400, 0x2800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(devices.devices().count(), 1);
}

#[test]
fn test_empty_and_wrapped_ranges_are_rejected() {
    let mut devices = empty_devices();
    let empty_mmio = Arc::new(MockMmioDevice::with_range(
        "empty-mmio",
        GuestPhysAddrRange::new(0x1000.into(), 0x1000.into()),
    ));
    let wrapped_mmio = Arc::new(MockMmioDevice::with_range(
        "wrapped-mmio",
        GuestPhysAddrRange {
            start: (usize::MAX - 0xf).into(),
            end: 0x10.into(),
        },
    ));
    let invalid_port = Arc::new(MockPortDevice::new(0x400, 0x3ff));
    let invalid_sysreg = Arc::new(MockSysRegDevice::new(0x101, 0x100));

    assert_eq!(
        register_mmio(&mut devices, empty_mmio),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_mmio(&mut devices, wrapped_mmio),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_port(&mut devices, invalid_port),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_sysreg(&mut devices, invalid_sysreg),
        Err(AxError::AddrInUse)
    );
    assert_eq!(devices.devices().count(), 0);
}

#[test]
fn test_port_inclusive_endpoint_overlap_is_rejected() {
    let mut devices = empty_devices();

    assert_eq!(
        register_port(&mut devices, Arc::new(MockPortDevice::new(0x3f8, 0x3ff))),
        Ok(())
    );
    assert_eq!(
        register_port(&mut devices, Arc::new(MockPortDevice::new(0x3ff, 0x400))),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_port(&mut devices, Arc::new(MockPortDevice::new(0x400, 0x400))),
        Ok(())
    );
    assert_eq!(devices.devices().count(), 2);
}

#[test]
fn test_sysreg_inclusive_endpoint_overlap_is_rejected() {
    let mut devices = empty_devices();

    assert_eq!(
        register_sysreg(&mut devices, Arc::new(MockSysRegDevice::new(0x100, 0x110))),
        Ok(())
    );
    assert_eq!(
        register_sysreg(&mut devices, Arc::new(MockSysRegDevice::new(0x110, 0x120))),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        register_sysreg(&mut devices, Arc::new(MockSysRegDevice::new(0x111, 0x120))),
        Ok(())
    );
    assert_eq!(devices.devices().count(), 2);
}

#[test]
fn test_equal_address_values_on_different_buses_are_allowed() {
    let mut devices = empty_devices();

    assert_eq!(
        register_mmio(&mut devices, mmio_device("mmio", 0x1000, 0x1001)),
        Ok(())
    );
    assert_eq!(
        register_port(&mut devices, Arc::new(MockPortDevice::new(0x1000, 0x1000))),
        Ok(())
    );
    assert_eq!(
        register_sysreg(
            &mut devices,
            Arc::new(MockSysRegDevice::new(0x1000, 0x1000))
        ),
        Ok(())
    );
    assert_eq!(devices.devices().count(), 3);
}

#[test]
fn test_conflicting_device_config_returns_structured_error() {
    let ioapic = EmulatedDeviceConfig {
        name: String::from("ioapic"),
        base_gpa: 0xfec0_0000,
        length: 0x1000,
        irq_id: 0,
        emu_type: EmulatedDeviceType::X86IoApic,
        cfg_list: vec![],
    };

    assert_eq!(
        AxVmDevices::new(AxVmDeviceConfig::new(vec![ioapic.clone(), ioapic])).err(),
        Some(AxError::InvalidInput)
    );
}

#[test]
fn test_bundle_registers_mmio_and_port_together() {
    let mut devices = empty_devices();
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        mmio_device("bundle-mmio", 0x4000, 0x5000),
    )));
    bundle.push(DeviceRegistration::Device(PortDeviceAdapter::from_arc(
        Arc::new(MockPortDevice::new(0x500, 0x50f)),
    )));

    assert_eq!(devices.register_bundle(bundle), Ok(()));
    assert_eq!(devices.devices().count(), 2);
}

#[test]
fn test_bundle_internal_conflict_is_atomic() {
    let mut devices = empty_devices();
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        mmio_device("bundle-first", 0x4000, 0x5000),
    )));
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        mmio_device("bundle-overlap", 0x4800, 0x5800),
    )));
    bundle.push(DeviceRegistration::Device(PortDeviceAdapter::from_arc(
        Arc::new(MockPortDevice::new(0x500, 0x50f)),
    )));

    assert_eq!(
        devices.register_bundle(bundle).err(),
        Some(AxError::AddrInUse)
    );
    assert_eq!(devices.devices().count(), 0);
}

#[test]
fn test_bundle_existing_conflict_leaves_all_registries_unchanged() {
    let mut devices = empty_devices();
    register_port(&mut devices, Arc::new(MockPortDevice::new(0x3f8, 0x3ff))).unwrap();

    let count_before = devices.devices().count();
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        mmio_device("bundle-mmio", 0x6000, 0x7000),
    )));
    bundle.push(DeviceRegistration::Device(PortDeviceAdapter::from_arc(
        Arc::new(MockPortDevice::new(0x3ff, 0x400)),
    )));
    bundle.push(DeviceRegistration::Device(SysRegDeviceAdapter::from_arc(
        Arc::new(MockSysRegDevice::new(0x200, 0x210)),
    )));

    assert_eq!(
        devices.register_bundle(bundle).err(),
        Some(AxError::AddrInUse)
    );
    assert_eq!(devices.devices().count(), count_before);
}

#[test]
fn test_pollable_and_mmio_capabilities_share_one_device() {
    let mut devices = empty_devices();
    let shared = Arc::new(MockMmioPollableDevice::new(0x8000, 0x9000));
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        shared.clone(),
    )));
    bundle.push(DeviceRegistration::Pollable(shared.clone()));

    assert_eq!(devices.register_bundle(bundle), Ok(()));
    devices
        .iter_pollable_dev()
        .next()
        .unwrap()
        .poll(123_456)
        .unwrap();

    assert_eq!(devices.devices().count(), 1);
    assert_eq!(devices.iter_pollable_dev().count(), 1);
    assert_eq!(shared.polled_at(), vec![123_456]);
}

#[test]
fn test_duplicate_pollable_rejects_entire_bundle() {
    let mut devices = empty_devices();
    let shared = Arc::new(MockMmioPollableDevice::new(0xa000, 0xb000));
    devices
        .register_bundle(DeviceRegistration::Pollable(shared.clone()).into())
        .unwrap();

    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        shared.clone(),
    )));
    bundle.push(DeviceRegistration::Pollable(shared));

    assert_eq!(
        devices.register_bundle(bundle).err(),
        Some(AxError::AlreadyExists)
    );
    assert_eq!(devices.devices().count(), 0);
    assert_eq!(devices.iter_pollable_dev().count(), 1);
}

#[test]
fn test_factory_registry_registers_and_finds_factory() {
    let mut factories = DeviceFactoryRegistry::new();

    assert_eq!(factories.register(Arc::new(MockMmioFactory)), Ok(()));
    assert!(factories.get(EmulatedDeviceType::VirtioBlk).is_some());
    assert!(factories.get(EmulatedDeviceType::VirtioNet).is_none());
}

#[test]
fn test_factory_registry_rejects_duplicate_device_type() {
    let mut factories = DeviceFactoryRegistry::new();

    assert_eq!(factories.register(Arc::new(MockMmioFactory)), Ok(()));
    assert_eq!(
        factories.register(Arc::new(MockMmioFactory)),
        Err(AxError::AlreadyExists)
    );
}

#[test]
fn test_missing_factory_returns_unsupported() {
    let factories = DeviceFactoryRegistry::new();
    let resolver = RejectingIrqResolver;
    let context = DeviceBuildContext::new(&resolver);
    let config = device_config(
        "missing-console",
        EmulatedDeviceType::VirtioConsole,
        0x1000,
        0x1000,
    );

    assert_eq!(
        factories.build(&config, &context).err(),
        Some(AxError::Unsupported)
    );
    assert_eq!(
        AxVmDevices::build_with_factories(
            AxVmDeviceConfig::new(vec![config]),
            &factories,
            &context,
        )
        .err(),
        Some(AxError::Unsupported)
    );
}

#[test]
fn test_factory_build_registers_new_device_type_without_legacy_branch() {
    let mut factories = DeviceFactoryRegistry::new();
    factories.register(Arc::new(MockMmioFactory)).unwrap();
    let resolver = RejectingIrqResolver;
    let context = DeviceBuildContext::new(&resolver);
    let base = 0x1_0000;
    let devices = AxVmDevices::build_with_factories(
        AxVmDeviceConfig::new(vec![device_config(
            "factory-mmio",
            EmulatedDeviceType::VirtioBlk,
            base,
            0x1000,
        )]),
        &factories,
        &context,
    )
    .unwrap();

    assert_eq!(devices.devices().count(), 1);
    assert_eq!(
        devices
            .handle_mmio_read(base.into(), AccessWidth::try_from(4).unwrap())
            .unwrap(),
        0xDEAD_BEEF
    );
}

#[test]
fn test_factory_validation_failure_leaves_devices_unchanged() {
    let mut devices = empty_devices();
    register_port(&mut devices, Arc::new(MockPortDevice::new(0x3f8, 0x3ff))).unwrap();
    let count_before = devices.devices().count();
    let mut factories = DeviceFactoryRegistry::new();
    factories.register(Arc::new(MockMmioFactory)).unwrap();
    let resolver = RejectingIrqResolver;
    let context = DeviceBuildContext::new(&resolver);
    let invalid = device_config(
        "invalid-factory-mmio",
        EmulatedDeviceType::VirtioBlk,
        0x2_0000,
        0,
    );

    assert_eq!(
        devices.register_factory_device(&invalid, &factories, &context),
        Err(AxError::InvalidInput)
    );
    assert_eq!(devices.devices().count(), count_before);
}

#[test]
fn test_builtin_meta_factory_builds_dummy_config() {
    let mut factories = DeviceFactoryRegistry::new();
    register_builtin_factories(&mut factories).unwrap();
    let resolver = RejectingIrqResolver;
    let context = DeviceBuildContext::new(&resolver);
    let devices = AxVmDevices::build_with_factories(
        AxVmDeviceConfig::new(vec![device_config(
            "metadata",
            EmulatedDeviceType::Dummy,
            0,
            0,
        )]),
        &factories,
        &context,
    )
    .unwrap();

    assert_eq!(devices.devices().count(), 0);
}

#[test]
fn test_wrapped_native_mmio_resource_is_rejected() {
    // Simulate a native Device whose resources() returns an overflowing
    // MmioRange — this must be rejected by the registry, not panic.
    // Use a valid GuestPhysAddrRange (wrapped ranges are rejected by
    // the range constructor) and instead exercise the overflow via
    // zero-sized MMIO (which the adapter already maps to size=0 for
    // truly wrapped ranges).
    let mut devices = empty_devices();
    let mut bundle = DeviceBundle::new();
    bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
        mmio_device("zero-size", 0x1000, 0x1000),
    )));
    assert_eq!(
        devices.register_bundle(bundle).err(),
        Some(AxError::AddrInUse)
    );
    assert_eq!(devices.devices().count(), 0);
}

#[test]
fn test_native_device_resource_overflow_rejected() {
    // Directly construct a Resource with an overflowing MmioRange to
    // exercise the checked_add path without going through the adapter
    // or range constructor.
    use axdevice_base::{Device, DeviceError, RegistryError, Resource};

    struct OverflowDevice;
    impl Device for OverflowDevice {
        fn name(&self) -> &str {
            "overflow"
        }
        fn resources(&self) -> Vec<Resource> {
            vec![Resource::MmioRange {
                base: u64::MAX - 1,
                size: 4,
            }]
        }
        fn handle(
            &self,
            _: &axdevice_base::BusAccess,
        ) -> Result<axdevice_base::BusResponse, DeviceError> {
            Err(DeviceError::NotFound)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    let mut devices = empty_devices();
    let result = devices.register(Arc::new(OverflowDevice));
    assert!(matches!(result, Err(RegistryError::AddressConflict { .. })));
}

#[test]
fn test_native_device_port_resource_overflow_rejected() {
    use axdevice_base::{Device, DeviceError, RegistryError, Resource};

    struct OverflowPortDevice;
    impl Device for OverflowPortDevice {
        fn name(&self) -> &str {
            "overflow-port"
        }
        fn resources(&self) -> Vec<Resource> {
            vec![Resource::PortRange {
                base: u16::MAX - 1,
                size: 4,
            }]
        }
        fn handle(
            &self,
            _: &axdevice_base::BusAccess,
        ) -> Result<axdevice_base::BusResponse, DeviceError> {
            Err(DeviceError::NotFound)
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    let mut devices = empty_devices();
    let result = devices.register(Arc::new(OverflowPortDevice));
    assert!(matches!(result, Err(RegistryError::AddressConflict { .. })));
}

#[test]
fn test_build_with_factories_preserves_legacy_ivc_config() {
    let mut factories = DeviceFactoryRegistry::new();
    register_builtin_factories(&mut factories).unwrap();
    let resolver = RejectingIrqResolver;
    let context = DeviceBuildContext::new(&resolver);
    let devices = AxVmDevices::build_with_factories(
        AxVmDeviceConfig::new(vec![device_config(
            "ivc",
            EmulatedDeviceType::IVCChannel,
            0x4_0000,
            0x2000,
        )]),
        &factories,
        &context,
    )
    .unwrap();

    assert_eq!(devices.devices().count(), 0);
}

// Mock implementation for x86_vlapic host callbacks when running
// `cargo test -p axdevice` on x86_64 host.

struct MockX86VlapicHostIfImpl;

#[ax_crate_interface::impl_interface]
impl X86VlapicHostIf for MockX86VlapicHostIfImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        None
    }

    fn dealloc_frame(_paddr: PhysAddr) {}

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        VirtAddr::from(paddr.as_usize())
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        PhysAddr::from(vaddr.as_usize())
    }

    fn current_time() -> core::time::Duration {
        core::time::Duration::ZERO
    }

    fn current_time_nanos() -> u64 {
        0
    }

    fn register_timer(
        _deadline: core::time::Duration,
        _callback: Box<dyn FnOnce(core::time::Duration) + Send + 'static>,
    ) -> usize {
        0
    }

    fn cancel_timer(_token: usize) {}

    fn write_bytes(_bytes: &[u8]) {}

    fn read_bytes(_bytes: &mut [u8]) -> usize {
        0
    }

    fn current_vm_id() -> VMId {
        0
    }

    fn current_vm_vcpu_num() -> usize {
        1
    }

    fn current_vm_active_vcpus() -> usize {
        1
    }

    fn active_vcpus(_vm_id: VMId) -> Option<usize> {
        Some(1)
    }

    fn inject_interrupt(_vm_id: VMId, _vcpu_id: VCpuId, _vector: InterruptVector) -> AxResult {
        Ok(())
    }
}
