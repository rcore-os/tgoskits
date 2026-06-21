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
    rc::Rc,
    sync::{Arc, Mutex},
};

use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice::{
    AxVmDeviceConfig, AxVmDevices, BusAccess, BusAddress, BusKind, BusOp, BusResponse,
    DeviceBuildContext, DeviceCapabilities, DeviceError, DeviceFactory, DeviceFactoryCatalog,
    DeviceFactoryRegister, DeviceId, DeviceOps, DeviceRegistry, DeviceResult, IrqLine, IrqSink,
    IrqTarget, LegacyDeviceAdapter, MsiMessage, PciBarKind, Resource,
};
use axdevice_base::{AccessWidth, BaseDeviceOps, Port, PortRange, SysRegAddr, SysRegAddrRange};
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptVector,
    VCpuId, VMId,
};
use x86_vlapic::host::X86VlapicHostIf;

struct MockMmioDevice {
    name: String,
    range: GuestPhysAddrRange,
    last_write: Mutex<Option<(usize, usize)>>,
}

impl MockMmioDevice {
    fn new(name: &str, base: usize, len: usize) -> Self {
        let start = GuestPhysAddr::from(base);
        let end = GuestPhysAddr::from(base + len);

        Self {
            name: String::from(name),
            range: GuestPhysAddrRange::new(start, end),
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

#[test]
fn test_mmio_dispatch_functionality() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);

    let base_addr = 0x1000_0000;
    let dev_size = 0x1000;
    let mock_dev = Arc::new(MockMmioDevice::new("TestDev", base_addr, dev_size));

    devices.add_mmio_dev(mock_dev.clone());

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
    let devices = AxVmDevices::new(config);

    let invalid_addr = GuestPhysAddr::from(0x9999_9999);
    let width = AccessWidth::try_from(4).unwrap();

    assert!(devices.handle_mmio_read(invalid_addr, width).is_err());
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

#[derive(Debug)]
struct AbstractMockDevice {
    id: DeviceId,
    name: &'static str,
    resources: Vec<Resource>,
    capabilities: DeviceCapabilities,
}

impl DeviceOps for AbstractMockDevice {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.capabilities
    }

    fn access(&self, access: BusAccess) -> Result<BusResponse, DeviceError> {
        match access.op {
            BusOp::Read => Ok(BusResponse::Read { value: 0x55aa }),
            BusOp::Write { .. } => Ok(BusResponse::Write),
        }
    }
}

#[derive(Debug)]
struct NativeMmioCounter {
    id: DeviceId,
    name: &'static str,
    range: GuestPhysAddrRange,
    resources: Vec<Resource>,
    value: Mutex<usize>,
}

impl NativeMmioCounter {
    fn new(id: usize, base: usize, len: usize, initial: usize) -> Self {
        let range =
            GuestPhysAddrRange::new(GuestPhysAddr::from(base), GuestPhysAddr::from(base + len));

        Self {
            id: DeviceId::new(id),
            name: "native-mmio-counter",
            range,
            resources: vec![Resource::Mmio(range)],
            value: Mutex::new(initial),
        }
    }

    fn value(&self) -> usize {
        *self.value.lock().unwrap()
    }
}

struct TestBuildContext {
    next_id: usize,
}

impl DeviceBuildContext for TestBuildContext {
    fn alloc_device_id(&mut self) -> DeviceId {
        let id = DeviceId::new(self.next_id);
        self.next_id += 1;
        id
    }
}

struct MultiCounterFactory;

static MULTI_COUNTER_FACTORY: MultiCounterFactory = MultiCounterFactory;
static DUPLICATE_MULTI_COUNTER_FACTORY: MultiCounterFactory = MultiCounterFactory;

impl DeviceFactory for MultiCounterFactory {
    fn ty(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GPPTRedistributor
    }

    fn build(
        &self,
        ctx: &mut dyn DeviceBuildContext,
        config: &EmulatedDeviceConfig,
    ) -> DeviceResult<Vec<Rc<dyn DeviceOps>>> {
        let count = config.cfg_list.first().copied().unwrap_or(1);
        let stride = config.cfg_list.get(1).copied().unwrap_or(config.length);
        let mut devices: Vec<Rc<dyn DeviceOps>> = Vec::new();

        for index in 0..count {
            let id = ctx.alloc_device_id();
            let base = config.base_gpa + index * stride;
            devices.push(Rc::new(NativeMmioCounter::new(
                id.raw(),
                base,
                config.length,
                index,
            )));
        }

        Ok(devices)
    }
}

impl DeviceOps for NativeMmioCounter {
    fn id(&self) -> DeviceId {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities::none()
    }

    fn access(&self, access: BusAccess) -> Result<BusResponse, DeviceError> {
        let addr = match access.addr {
            BusAddress::Mmio(addr) if access.kind == BusKind::Mmio => addr,
            _ => {
                return Err(DeviceError::BusAddressMismatch {
                    kind: BusKind::Mmio,
                    address: access.addr,
                });
            }
        };

        if !self.range.contains(addr) {
            return Err(DeviceError::AddressOutOfRange {
                kind: BusKind::Mmio,
                address: access.addr,
            });
        }

        match access.op {
            BusOp::Read => Ok(BusResponse::Read {
                value: self.value(),
            }),
            BusOp::Write { value } => {
                *self.value.lock().unwrap() = value;
                Ok(BusResponse::Write)
            }
        }
    }
}

#[test]
fn device_ops_exposes_identity_resources_and_capabilities() {
    let mmio_range = GuestPhysAddrRange::new(
        GuestPhysAddr::from(0x1000_0000),
        GuestPhysAddr::from(0x1000_1000),
    );
    let device = AbstractMockDevice {
        id: DeviceId::new(7),
        name: "mock-net",
        resources: vec![
            Resource::Mmio(mmio_range),
            Resource::Irq(IrqLine::new(32)),
            Resource::Msi { vectors: 4 },
            Resource::Dma,
            Resource::PciBar {
                index: 0,
                kind: PciBarKind::Mmio64 {
                    prefetchable: false,
                },
            },
        ],
        capabilities: DeviceCapabilities {
            msi: true,
            msix: false,
            dma: true,
            pci_bar: true,
            reset: true,
            suspend: false,
            resume: false,
        },
    };

    assert_eq!(device.id().raw(), 7);
    assert_eq!(device.name(), "mock-net");
    assert!(matches!(device.resources()[0], Resource::Mmio(range) if range == mmio_range));
    assert!(matches!(device.resources()[1], Resource::Irq(line) if line.number() == 32));
    assert!(device.capabilities().msi);
    assert!(device.capabilities().dma);
    assert!(device.reset().is_ok());
    assert!(device.suspend().is_ok());
    assert!(device.resume().is_ok());
}

#[test]
fn bus_access_constructors_cover_all_current_exit_buses() {
    let mmio_addr = GuestPhysAddr::from(0x2000_0000);
    let mmio_read = BusAccess::mmio_read(mmio_addr, AccessWidth::Dword);
    assert_eq!(mmio_read.kind, BusKind::Mmio);
    assert_eq!(mmio_read.addr, BusAddress::Mmio(mmio_addr));
    assert_eq!(mmio_read.addr.kind(), BusKind::Mmio);
    assert_eq!(mmio_read.op, BusOp::Read);

    let mmio_write = BusAccess::mmio_write(mmio_addr, AccessWidth::Qword, 0x1234);
    assert!(matches!(mmio_write.op, BusOp::Write { value: 0x1234 }));

    let pio_read = BusAccess::pio_read(axdevice_base::Port::new(0x3f8), AccessWidth::Byte);
    assert_eq!(pio_read.kind, BusKind::Pio);
    assert_eq!(pio_read.addr.kind(), BusKind::Pio);

    let pio_write = BusAccess::pio_write(axdevice_base::Port::new(0x40), AccessWidth::Word, 0x12);
    assert!(matches!(pio_write.op, BusOp::Write { value: 0x12 }));

    let sysreg = axdevice_base::SysRegAddr::new(0x10);
    let sysreg_read = BusAccess::sysreg_read(sysreg, AccessWidth::Qword);
    assert_eq!(sysreg_read.kind, BusKind::SysReg);
    assert_eq!(sysreg_read.addr, BusAddress::SysReg(sysreg));

    let sysreg_write = BusAccess::sysreg_write(sysreg, AccessWidth::Qword, 0x88);
    assert!(matches!(sysreg_write.op, BusOp::Write { value: 0x88 }));
}

#[test]
fn resources_cover_mmio_pio_sysreg_irq_msi_dma_and_pci_bar() {
    let resources = [
        Resource::Mmio(GuestPhysAddrRange::new(
            GuestPhysAddr::from(0x3000_0000),
            GuestPhysAddr::from(0x3000_1000),
        )),
        Resource::Pio(axdevice_base::PortRange::new(
            axdevice_base::Port::new(0x40),
            axdevice_base::Port::new(0x43),
        )),
        Resource::SysReg(axdevice_base::SysRegAddrRange::new(
            axdevice_base::SysRegAddr::new(0x20),
            axdevice_base::SysRegAddr::new(0x23),
        )),
        Resource::Irq(IrqLine::new(4)),
        Resource::Msi { vectors: 8 },
        Resource::Dma,
        Resource::PciBar {
            index: 1,
            kind: PciBarKind::Pio,
        },
    ];

    assert!(matches!(resources[0], Resource::Mmio(_)));
    assert!(matches!(resources[1], Resource::Pio(_)));
    assert!(matches!(resources[2], Resource::SysReg(_)));
    assert!(matches!(resources[3], Resource::Irq(line) if line.number() == 4));
    assert!(matches!(resources[4], Resource::Msi { vectors: 8 }));
    assert!(matches!(resources[5], Resource::Dma));
    assert!(matches!(
        resources[6],
        Resource::PciBar {
            index: 1,
            kind: PciBarKind::Pio
        }
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IrqEvent {
    Raise(IrqLine),
    Lower(IrqLine),
    Pulse(IrqLine),
    Msi(MsiMessage),
    Eoi(IrqLine),
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
    fn raise(&self, line: IrqLine) -> Result<(), DeviceError> {
        self.events.lock().unwrap().push(IrqEvent::Raise(line));
        Ok(())
    }

    fn lower(&self, line: IrqLine) -> Result<(), DeviceError> {
        self.events.lock().unwrap().push(IrqEvent::Lower(line));
        Ok(())
    }

    fn pulse(&self, line: IrqLine) -> Result<(), DeviceError> {
        self.events.lock().unwrap().push(IrqEvent::Pulse(line));
        Ok(())
    }

    fn msi(&self, message: MsiMessage) -> Result<(), DeviceError> {
        self.events.lock().unwrap().push(IrqEvent::Msi(message));
        Ok(())
    }

    fn eoi(&self, line: IrqLine) -> Result<(), DeviceError> {
        self.events.lock().unwrap().push(IrqEvent::Eoi(line));
        Ok(())
    }
}

#[test]
fn irq_sink_records_semantic_interrupt_operations() {
    let sink = RecordingIrqSink::default();
    let line = IrqLine::new(9);
    let msi = MsiMessage::with_target(0xfee0_0000, 0x45, IrqTarget::Vcpu(0));

    sink.raise(line).unwrap();
    sink.lower(line).unwrap();
    sink.pulse(line).unwrap();
    sink.msi(msi).unwrap();
    sink.eoi(line).unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::Raise(line),
            IrqEvent::Lower(line),
            IrqEvent::Pulse(line),
            IrqEvent::Msi(msi),
            IrqEvent::Eoi(line),
        ]
    );
}

#[test]
fn device_error_variants_describe_access_failures() {
    let width_error = DeviceError::InvalidAccessWidth {
        width: AccessWidth::Qword,
    };
    assert!(matches!(
        width_error,
        DeviceError::InvalidAccessWidth { .. }
    ));

    let unsupported = DeviceError::UnsupportedOperation;
    assert_eq!(unsupported.to_string(), "unsupported device operation");

    let backend: DeviceError = ax_errno::AxError::Unsupported.into();
    assert!(matches!(backend, DeviceError::Backend(_)));
}

struct MockPortDevice {
    range: PortRange,
    last_write: Mutex<Option<(u16, usize)>>,
}

impl MockPortDevice {
    fn new(start: u16, end: u16) -> Self {
        Self {
            range: PortRange::new(Port::new(start), Port::new(end)),
            last_write: Mutex::new(None),
        }
    }

    fn get_last_write(&self) -> Option<(u16, usize)> {
        *self.last_write.lock().unwrap()
    }
}

impl BaseDeviceOps<PortRange> for MockPortDevice {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Console
    }

    fn address_range(&self) -> PortRange {
        self.range
    }

    fn handle_read(&self, _addr: Port, _width: AccessWidth) -> AxResult<usize> {
        Ok(0x44)
    }

    fn handle_write(&self, port: Port, _width: AccessWidth, val: usize) -> AxResult {
        *self.last_write.lock().unwrap() = Some((port.number(), val));
        Ok(())
    }
}

struct MockSysRegDevice {
    range: SysRegAddrRange,
    last_write: Mutex<Option<(usize, usize)>>,
}

impl MockSysRegDevice {
    fn new(start: usize, end: usize) -> Self {
        Self {
            range: SysRegAddrRange::new(SysRegAddr::new(start), SysRegAddr::new(end)),
            last_write: Mutex::new(None),
        }
    }

    fn get_last_write(&self) -> Option<(usize, usize)> {
        *self.last_write.lock().unwrap()
    }
}

impl BaseDeviceOps<SysRegAddrRange> for MockSysRegDevice {
    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn address_range(&self) -> SysRegAddrRange {
        self.range
    }

    fn handle_read(&self, _addr: SysRegAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0x99)
    }

    fn handle_write(&self, addr: SysRegAddr, _width: AccessWidth, val: usize) -> AxResult {
        *self.last_write.lock().unwrap() = Some((addr.addr(), val));
        Ok(())
    }
}

fn mmio_resource(start: usize, end: usize) -> Resource {
    Resource::Mmio(GuestPhysAddrRange::new(
        GuestPhysAddr::from(start),
        GuestPhysAddr::from(end),
    ))
}

fn mock_device(id: usize, resources: Vec<Resource>) -> Rc<AbstractMockDevice> {
    Rc::new(AbstractMockDevice {
        id: DeviceId::new(id),
        name: "registry-mock",
        resources,
        capabilities: DeviceCapabilities::none(),
    })
}

#[test]
fn device_factory_catalog_builds_multiple_native_devices() {
    let factory = MultiCounterFactory;
    let factories: [&dyn DeviceFactory; 1] = [&factory];
    let catalog = DeviceFactoryCatalog::new(&factories);

    assert!(
        catalog
            .find(EmulatedDeviceType::GPPTRedistributor)
            .is_some()
    );
    assert!(catalog.find(EmulatedDeviceType::GPPTITS).is_none());

    let config = EmulatedDeviceConfig {
        name: "counter".into(),
        base_gpa: 0xb000_0000,
        length: 0x1000,
        irq_id: 0,
        emu_type: EmulatedDeviceType::GPPTRedistributor,
        cfg_list: vec![2, 0x1000],
    };
    let mut ctx = TestBuildContext { next_id: 200 };
    let devices = catalog
        .find(config.emu_type)
        .unwrap()
        .build(&mut ctx, &config)
        .unwrap();

    assert_eq!(devices.len(), 2);

    let mut registry = DeviceRegistry::new();
    for device in devices {
        registry.register_device(device).unwrap();
    }
    assert_eq!(registry.device_count(), 2);
    assert_eq!(registry.mmio_route_count(), 2);

    assert_eq!(
        registry
            .dispatch(BusAccess::mmio_read(
                GuestPhysAddr::from(0xb000_0000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0 }
    );
    assert_eq!(
        registry
            .dispatch(BusAccess::mmio_read(
                GuestPhysAddr::from(0xb000_1000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 1 }
    );
}

#[test]
fn device_factory_catalog_finds_registered_factories() {
    let registers = [DeviceFactoryRegister::new(
        "multi-counter",
        &MULTI_COUNTER_FACTORY,
    )];
    let catalog = DeviceFactoryCatalog::from_registers(&registers);

    assert!(
        catalog
            .find(EmulatedDeviceType::GPPTRedistributor)
            .is_some()
    );
    assert!(
        catalog
            .find_unique(EmulatedDeviceType::GPPTRedistributor)
            .unwrap()
            .is_some()
    );
    assert!(catalog.find(EmulatedDeviceType::GPPTITS).is_none());
}

#[test]
fn device_factory_catalog_rejects_duplicate_registered_factories() {
    let registers = [
        DeviceFactoryRegister::new("multi-counter", &MULTI_COUNTER_FACTORY),
        DeviceFactoryRegister::new("duplicate-counter", &DUPLICATE_MULTI_COUNTER_FACTORY),
    ];
    let catalog = DeviceFactoryCatalog::from_registers(&registers);

    let error = match catalog.find_unique(EmulatedDeviceType::GPPTRedistributor) {
        Ok(_) => panic!("duplicate factories should be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, DeviceError::Backend(_)));
}

#[test]
fn registry_registers_routes_and_dispatches_mock_devices() {
    let mut registry = DeviceRegistry::new();
    registry
        .register_device(mock_device(
            1,
            vec![mmio_resource(0x4000_0000, 0x4000_1000)],
        ))
        .unwrap();
    registry
        .register_device(mock_device(
            2,
            vec![Resource::Pio(PortRange::new(
                Port::new(0x80),
                Port::new(0x8f),
            ))],
        ))
        .unwrap();
    registry
        .register_device(mock_device(
            3,
            vec![Resource::SysReg(SysRegAddrRange::new(
                SysRegAddr::new(0x100),
                SysRegAddr::new(0x10f),
            ))],
        ))
        .unwrap();
    registry
        .register_device(mock_device(
            4,
            vec![Resource::Irq(IrqLine::new(10)), Resource::Dma],
        ))
        .unwrap();

    assert_eq!(registry.device_count(), 4);
    assert_eq!(registry.mmio_route_count(), 1);
    assert_eq!(registry.pio_route_count(), 1);
    assert_eq!(registry.sysreg_route_count(), 1);
    assert!(registry.find_device(DeviceId::new(3)).is_some());

    let mmio = registry
        .dispatch(BusAccess::mmio_read(
            GuestPhysAddr::from(0x4000_0004),
            AccessWidth::Dword,
        ))
        .unwrap();
    assert_eq!(mmio, BusResponse::Read { value: 0x55aa });

    let pio = registry
        .dispatch(BusAccess::pio_write(
            Port::new(0x81),
            AccessWidth::Byte,
            0xaa,
        ))
        .unwrap();
    assert_eq!(pio, BusResponse::Write);

    let sysreg = registry
        .dispatch(BusAccess::sysreg_read(
            SysRegAddr::new(0x101),
            AccessWidth::Qword,
        ))
        .unwrap();
    assert_eq!(sysreg, BusResponse::Read { value: 0x55aa });
}

#[test]
fn registry_dispatches_native_mmio_counter_without_legacy_adapter() {
    let mut registry = DeviceRegistry::new();
    let counter = Rc::new(NativeMmioCounter::new(50, 0x4100_0000, 0x1000, 0x11));

    assert_eq!(
        registry.register_device(counter.clone()).unwrap(),
        DeviceId::new(50)
    );
    assert_eq!(registry.device_count(), 1);
    assert_eq!(registry.mmio_route_count(), 1);
    assert_eq!(registry.pio_route_count(), 0);
    assert_eq!(registry.sysreg_route_count(), 0);

    assert_eq!(
        registry
            .dispatch(BusAccess::mmio_read(
                GuestPhysAddr::from(0x4100_0008),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0x11 }
    );

    assert_eq!(
        registry
            .dispatch(BusAccess::mmio_write(
                GuestPhysAddr::from(0x4100_0008),
                AccessWidth::Dword,
                0x22,
            ))
            .unwrap(),
        BusResponse::Write
    );
    assert_eq!(counter.value(), 0x22);

    assert_eq!(
        registry
            .dispatch(BusAccess::mmio_read(
                GuestPhysAddr::from(0x4100_0008),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0x22 }
    );

    let out_of_range = counter
        .access(BusAccess::mmio_read(
            GuestPhysAddr::from(0x4100_2000),
            AccessWidth::Dword,
        ))
        .unwrap_err();
    assert!(matches!(
        out_of_range,
        DeviceError::AddressOutOfRange { .. }
    ));

    let mismatch = counter
        .access(BusAccess::pio_read(Port::new(0x80), AccessWidth::Byte))
        .unwrap_err();
    assert!(matches!(mismatch, DeviceError::BusAddressMismatch { .. }));

    let duplicate = Rc::new(NativeMmioCounter::new(50, 0x4100_2000, 0x1000, 0));
    let err = registry.register_device(duplicate).unwrap_err();
    assert!(matches!(err, DeviceError::DuplicateDeviceId { id } if id == DeviceId::new(50)));

    let overlapping = Rc::new(NativeMmioCounter::new(51, 0x4100_0800, 0x1000, 0));
    let err = registry.register_device(overlapping).unwrap_err();
    assert!(matches!(err, DeviceError::ResourceConflict { .. }));

    let adjacent = Rc::new(NativeMmioCounter::new(52, 0x4100_1000, 0x1000, 0));
    assert_eq!(
        registry.register_device(adjacent).unwrap(),
        DeviceId::new(52)
    );
}

#[test]
fn registry_rejects_duplicate_device_ids() {
    let mut registry = DeviceRegistry::new();
    registry
        .register_device(mock_device(
            1,
            vec![mmio_resource(0x5000_0000, 0x5000_1000)],
        ))
        .unwrap();

    let err = registry
        .register_device(mock_device(
            1,
            vec![mmio_resource(0x5000_1000, 0x5000_2000)],
        ))
        .unwrap_err();
    assert!(matches!(
        err,
        DeviceError::DuplicateDeviceId { id } if id == DeviceId::new(1)
    ));
}

#[test]
fn registry_rejects_overlapping_bus_resources() {
    let mut registry = DeviceRegistry::new();
    registry
        .register_device(mock_device(
            1,
            vec![mmio_resource(0x6000_0000, 0x6000_1000)],
        ))
        .unwrap();
    registry
        .register_device(mock_device(
            2,
            vec![mmio_resource(0x6000_1000, 0x6000_2000)],
        ))
        .unwrap();

    let err = registry
        .register_device(mock_device(
            3,
            vec![mmio_resource(0x6000_0800, 0x6000_1800)],
        ))
        .unwrap_err();
    assert!(matches!(err, DeviceError::ResourceConflict { .. }));

    registry
        .register_device(mock_device(
            4,
            vec![Resource::Pio(PortRange::new(
                Port::new(0x200),
                Port::new(0x20f),
            ))],
        ))
        .unwrap();
    let err = registry
        .register_device(mock_device(
            5,
            vec![Resource::Pio(PortRange::new(
                Port::new(0x20f),
                Port::new(0x210),
            ))],
        ))
        .unwrap_err();
    assert!(matches!(err, DeviceError::ResourceConflict { .. }));

    registry
        .register_device(mock_device(
            6,
            vec![Resource::SysReg(SysRegAddrRange::new(
                SysRegAddr::new(0x300),
                SysRegAddr::new(0x30f),
            ))],
        ))
        .unwrap();
    let err = registry
        .register_device(mock_device(
            7,
            vec![Resource::SysReg(SysRegAddrRange::new(
                SysRegAddr::new(0x30f),
                SysRegAddr::new(0x310),
            ))],
        ))
        .unwrap_err();
    assert!(matches!(err, DeviceError::ResourceConflict { .. }));
}

#[test]
fn registry_reports_misses_and_bus_address_mismatch_without_panicking() {
    let mut registry = DeviceRegistry::new();
    registry
        .register_device(mock_device(
            1,
            vec![mmio_resource(0x7000_0000, 0x7000_1000)],
        ))
        .unwrap();

    let miss = registry
        .dispatch(BusAccess::mmio_read(
            GuestPhysAddr::from(0x7000_2000),
            AccessWidth::Dword,
        ))
        .unwrap_err();
    assert!(matches!(miss, DeviceError::DeviceNotFound { .. }));

    let mismatch = registry
        .dispatch(BusAccess::new(
            BusKind::Mmio,
            BusAddress::Pio(Port::new(0x70)),
            AccessWidth::Byte,
            BusOp::Read,
        ))
        .unwrap_err();
    assert!(matches!(mismatch, DeviceError::BusAddressMismatch { .. }));
}

#[test]
fn legacy_device_adapter_forwards_to_existing_base_device_ops() {
    let mmio = Arc::new(MockMmioDevice::new("legacy-mmio", 0x8000_0000, 0x1000));
    let mmio_adapter = LegacyDeviceAdapter::mmio(
        DeviceId::new(1),
        "legacy-mmio".into(),
        vec![Resource::Mmio(mmio.address_range())],
        DeviceCapabilities::none(),
        mmio.clone(),
    );
    assert_eq!(
        mmio_adapter
            .access(BusAccess::mmio_read(
                GuestPhysAddr::from(0x8000_0000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0xDEAD_BEEF }
    );
    mmio_adapter
        .access(BusAccess::mmio_write(
            GuestPhysAddr::from(0x8000_0040),
            AccessWidth::Dword,
            0xbeef,
        ))
        .unwrap();
    assert_eq!(mmio.get_last_write(), Some((0x40, 0xbeef)));

    let pio = Arc::new(MockPortDevice::new(0x90, 0x9f));
    let pio_adapter = LegacyDeviceAdapter::pio(
        DeviceId::new(2),
        "legacy-pio".into(),
        vec![Resource::Pio(pio.address_range())],
        DeviceCapabilities::none(),
        pio.clone(),
    );
    assert_eq!(
        pio_adapter
            .access(BusAccess::pio_read(Port::new(0x90), AccessWidth::Byte))
            .unwrap(),
        BusResponse::Read { value: 0x44 }
    );
    pio_adapter
        .access(BusAccess::pio_write(
            Port::new(0x91),
            AccessWidth::Byte,
            0x33,
        ))
        .unwrap();
    assert_eq!(pio.get_last_write(), Some((0x91, 0x33)));

    let sysreg = Arc::new(MockSysRegDevice::new(0x400, 0x40f));
    let sysreg_adapter = LegacyDeviceAdapter::sysreg(
        DeviceId::new(3),
        "legacy-sysreg".into(),
        vec![Resource::SysReg(sysreg.address_range())],
        DeviceCapabilities::none(),
        sysreg.clone(),
    );
    assert_eq!(
        sysreg_adapter
            .access(BusAccess::sysreg_read(
                SysRegAddr::new(0x400),
                AccessWidth::Qword
            ))
            .unwrap(),
        BusResponse::Read { value: 0x99 }
    );
    sysreg_adapter
        .access(BusAccess::sysreg_write(
            SysRegAddr::new(0x401),
            AccessWidth::Qword,
            0x77,
        ))
        .unwrap();
    assert_eq!(sysreg.get_last_write(), Some((0x401, 0x77)));

    let mismatch = sysreg_adapter
        .access(BusAccess::pio_read(Port::new(0x90), AccessWidth::Byte))
        .unwrap_err();
    assert!(matches!(mismatch, DeviceError::BusAddressMismatch { .. }));
}

#[test]
fn ax_vm_devices_dispatch_bus_access_uses_new_registry_without_breaking_old_path() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);
    let mmio = Arc::new(MockMmioDevice::new("dual-mmio", 0x9000_0000, 0x1000));
    devices.add_mmio_dev(mmio.clone());

    assert_eq!(
        devices
            .handle_mmio_read(GuestPhysAddr::from(0x9000_0000), AccessWidth::Dword)
            .unwrap(),
        0xDEAD_BEEF
    );
    assert_eq!(
        devices
            .dispatch_bus_access(BusAccess::mmio_read(
                GuestPhysAddr::from(0x9000_0000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0xDEAD_BEEF }
    );

    let pio = Arc::new(MockPortDevice::new(0xa0, 0xaf));
    devices.add_port_dev(pio.clone());
    devices
        .dispatch_bus_access(BusAccess::pio_write(
            Port::new(0xa1),
            AccessWidth::Byte,
            0x55,
        ))
        .unwrap();
    assert_eq!(pio.get_last_write(), Some((0xa1, 0x55)));

    let sysreg = Arc::new(MockSysRegDevice::new(0x500, 0x50f));
    devices.add_sys_reg_dev(sysreg.clone());
    devices
        .dispatch_bus_access(BusAccess::sysreg_write(
            SysRegAddr::new(0x501),
            AccessWidth::Qword,
            0x66,
        ))
        .unwrap();
    assert_eq!(sysreg.get_last_write(), Some((0x501, 0x66)));
}

#[test]
fn ax_vm_devices_registers_native_device_without_old_vec_membership() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);
    let counter = Rc::new(NativeMmioCounter::new(100, 0xa000_0000, 0x1000, 0x5));

    assert_eq!(devices.iter_mmio_dev().count(), 0);
    assert_eq!(
        devices.register_device(counter.clone()).unwrap(),
        DeviceId::new(100)
    );
    assert_eq!(devices.iter_mmio_dev().count(), 0);

    assert_eq!(
        devices
            .dispatch_bus_access(BusAccess::mmio_read(
                GuestPhysAddr::from(0xa000_0000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0x5 }
    );

    devices
        .dispatch_bus_access(BusAccess::mmio_write(
            GuestPhysAddr::from(0xa000_0004),
            AccessWidth::Dword,
            0x6,
        ))
        .unwrap();
    assert_eq!(counter.value(), 0x6);

    let duplicate = Rc::new(NativeMmioCounter::new(100, 0xa000_2000, 0x1000, 0));
    let err = devices.register_device(duplicate).unwrap_err();
    assert!(matches!(err, DeviceError::DuplicateDeviceId { id } if id == DeviceId::new(100)));

    let overlapping = Rc::new(NativeMmioCounter::new(101, 0xa000_0800, 0x1000, 0));
    let err = devices.register_device(overlapping).unwrap_err();
    assert!(matches!(err, DeviceError::ResourceConflict { .. }));

    let legacy = Arc::new(MockMmioDevice::new(
        "legacy-after-native",
        0xa000_2000,
        0x1000,
    ));
    devices.add_mmio_dev(legacy);
    assert_eq!(devices.iter_mmio_dev().count(), 1);
    assert_eq!(
        devices
            .dispatch_bus_access(BusAccess::mmio_read(
                GuestPhysAddr::from(0xa000_2000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0xDEAD_BEEF }
    );
    assert_eq!(
        devices
            .dispatch_bus_access(BusAccess::mmio_read(
                GuestPhysAddr::from(0xa000_0000),
                AccessWidth::Dword,
            ))
            .unwrap(),
        BusResponse::Read { value: 0x6 }
    );
}
