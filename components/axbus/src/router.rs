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

//! Central bus router — the top-level dispatch layer that replaces `AxVmDevices`.
//!
//! `BusRouter` owns a single `DeviceRegistry` and an `IrqRoutingTable`, providing:
//!
//! - **Address-based routing** via `route()`: dispatches guest access to the
//!   correct device across MMIO, PIO, and SysReg buses.
//! - **Interrupt injection** via `inject()`: routes through the interrupt table.
//! - **Device lifecycle** via `register()` / `unregister()`.`

use alloc::sync::Arc;

use crate::irq::{IrqMessage, IrqRoutingTable};
use crate::r#trait::*;
use crate::registry::DeviceRegistry;

/// Top-level bus router for a single VM.
pub struct BusRouter {
    registry: DeviceRegistry,
    irq_table: IrqRoutingTable,
}

impl BusRouter {
    /// Create a new, empty router.
    pub fn new() -> Self {
        Self {
            registry: DeviceRegistry::new(),
            irq_table: IrqRoutingTable::new(),
        }
    }

    /// Access the interrupt routing table for configuration.
    pub fn irq_table_mut(&mut self) -> &mut IrqRoutingTable {
        &mut self.irq_table
    }

    /// Access the interrupt routing table (read-only).
    pub fn irq_table(&self) -> &IrqRoutingTable {
        &self.irq_table
    }

    // ── Device lifecycle ──────────────────────────────────────────────

    /// Register a device. The device's `resources()` determine which bus(es)
    /// it is added to. Returns the assigned `DeviceId`.
    ///
    /// A single `DeviceRegistry` handles all bus types (MMIO, PIO, SysReg)
    /// internally via its interval trees, so multi-resource devices (e.g.,
    /// MMIO + PIO, or pure SysReg) are fully routable on all their buses.
    pub fn register(&mut self, dev: Arc<dyn VirtualDevice>) -> Result<DeviceId> {
        self.registry.register(dev)
    }

    /// Unregister a device by its ID.
    pub fn unregister(&mut self, id: DeviceId) -> Option<Arc<dyn VirtualDevice>> {
        self.registry.unregister(id)
    }

    // ── Access routing ────────────────────────────────────────────────

    /// Route a single guest bus access to the appropriate device.
    ///
    /// The `bus` parameter is passed through to `DeviceRegistry::handle_access`,
    /// which dispatches via the correct interval tree (mmio_tree, pio_tree)
    /// or iterates all devices for SysReg.
    pub fn route(&self, bus: BusKind, access: &BusAccess) -> BusResponse {
        self.registry.handle_access(bus, access)
    }

    // ── Interrupt injection ───────────────────────────────────────────

    /// Inject an interrupt by routing through the `IrqRoutingTable`.
    ///
    /// For legacy (line-based) interrupts, the table maps `IrqLine` to a
    /// controller device + pin. For MSI, the table maps the address to a
    /// controller, which decodes the address+data itself.
    pub fn inject(&self, msg: IrqMessage) -> Result<()> {
        match msg {
            IrqMessage::Legacy { line } => {
                // Route through the table — copy out entry fields first to
                // avoid borrowing self.irq_table and self.registries simultaneously.
                let (ctrl_id, pin, trigger, target) = {
                    let (_ctrl_id, entry) = self
                        .irq_table
                        .lookup_legacy(line)
                        .ok_or(DeviceError::NotFound)?;
                    (_ctrl_id, entry.controller_pin, entry.trigger, entry.target)
                };

                // Find the controller device (borrows self.registries)
                let ctrl = self
                    .find_interrupt_controller(ctrl_id)
                    .ok_or(DeviceError::NotFound)?;
                let ctrl_ops = ctrl.as_interrupt_controller().ok_or(DeviceError::NotFound)?;

                ctrl_ops.inject_irq(pin, trigger, target)
            }
            IrqMessage::Msi { addr, data } => {
                // Route by MSI address window
                let ctrl_id = self
                    .irq_table
                    .lookup_msi(addr)
                    .ok_or(DeviceError::NotFound)?;

                let ctrl = self
                    .find_interrupt_controller(ctrl_id)
                    .ok_or(DeviceError::NotFound)?;
                let ctrl_ops = ctrl.as_interrupt_controller().ok_or(DeviceError::NotFound)?;

                ctrl_ops.handle_msi(addr, data)
            }
        }
    }

    /// Deactivate a level-triggered interrupt.
    pub fn deactivate_irq(&self, line: IrqLine) -> Result<()> {
        // Single lookup (fixes double-lookup inconsistency).
        let (ctrl_id, entry) = self
            .irq_table
            .lookup_legacy(line)
            .ok_or(DeviceError::NotFound)?;

        let ctrl = self
            .find_interrupt_controller(ctrl_id)
            .ok_or(DeviceError::NotFound)?;
        let ctrl_ops = ctrl.as_interrupt_controller().ok_or(DeviceError::NotFound)?;

        ctrl_ops.deactivate_irq(entry.controller_pin)
    }

    /// Find a device that implements `InterruptControllerOps` by its DeviceId.
    fn find_interrupt_controller(&self, id: DeviceId) -> Option<Arc<dyn VirtualDevice>> {
        self.registry.get(id)
    }

    // ── Iteration ─────────────────────────────────────────────────────

    /// Iterate over all devices.
    pub fn iter_all(&self) -> impl Iterator<Item = (DeviceId, &Arc<dyn VirtualDevice>)> {
        self.registry.iter()
    }

    /// Number of registered devices.
    pub fn total_devices(&self) -> usize {
        self.registry.len()
    }
}

#[cfg(test)]
#[allow(missing_docs, dead_code)]
mod tests {
    use super::*;
    use crate::irq::{IrqMessage, TriggerMode};
    use crate::InterruptControllerOps;
    use core::any::Any;

    #[derive(Debug)]
    struct TestDevice;

    impl VirtualDevice for TestDevice {
        fn id(&self) -> DeviceId {
            DeviceId::from_u64(0)
        }
        fn name(&self) -> &str {
            "test"
        }
        fn resources(&self) -> &[Resource] {
            static RES: &[Resource] = &[Resource::Mmio(0x1000..0x2000)];
            RES
        }
        fn handle_access(&self, _bus: BusKind, _access: &BusAccess) -> BusResponse {
            BusResponse::Success(Some(0xff))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn test_router_register_and_route() {
        let mut router = BusRouter::new();
        let _id = router.register(Arc::new(TestDevice)).unwrap();

        let resp = router.route(
            BusKind::Mmio,
            &BusAccess::Read {
                addr: 0x1500,
                width: AccessWidth::U32,
            },
        );
        assert!(matches!(resp, BusResponse::Success(Some(0xff))));
    }

    #[test]
    fn test_router_route_no_device() {
        let router = BusRouter::new();
        let resp = router.route(
            BusKind::Mmio,
            &BusAccess::Read {
                addr: 0x9999,
                width: AccessWidth::U32,
            },
        );
        assert!(matches!(resp, BusResponse::NoDevice));
    }

    #[test]
    fn test_router_unregister() {
        let mut router = BusRouter::new();
        let id = router.register(Arc::new(TestDevice)).unwrap();

        assert!(matches!(
            router.route(
                BusKind::Mmio,
                &BusAccess::Read {
                    addr: 0x1500,
                    width: AccessWidth::U32,
                },
            ),
            BusResponse::Success(_)
        ));

        router.unregister(id);

        assert!(matches!(
            router.route(
                BusKind::Mmio,
                &BusAccess::Read {
                    addr: 0x1500,
                    width: AccessWidth::U32,
                },
            ),
            BusResponse::NoDevice
        ));
    }

    #[test]
    fn test_inject_no_controller() {
        let router = BusRouter::new();
        let result = router.inject(IrqMessage::leg(IrqLine(33)));
        assert!(matches!(result, Err(DeviceError::NotFound)));
    }

    #[test]
    fn test_total_devices() {
        let mut router = BusRouter::new();
        assert_eq!(router.total_devices(), 0);
        router.register(Arc::new(TestDevice)).unwrap();
        assert_eq!(router.total_devices(), 1);
    }

    #[test]
    fn test_iter_all() {
        let mut router = BusRouter::new();
        router.register(Arc::new(TestDevice)).unwrap();
        let count = router.iter_all().count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_route_different_bus_no_cross_contamination() {
        let mut router = BusRouter::new();
        router.register(Arc::new(TestDevice)).unwrap();
        let resp = router.route(
            BusKind::Pio,
            &BusAccess::Read { addr: 0x1500, width: AccessWidth::U32 },
        );
        assert!(matches!(resp, BusResponse::NoDevice));
    }

    // ── Multi-resource device ────────────────────────────────────────────

    #[derive(Debug)]
    struct MultiResDevice {
        resources: Vec<Resource>,
    }

    impl VirtualDevice for MultiResDevice {
        fn id(&self) -> DeviceId { DeviceId::from_u64(0) }
        fn name(&self) -> &str { "multi-res" }
        fn resources(&self) -> &[Resource] { &self.resources }
        fn handle_access(&self, _bus: BusKind, access: &BusAccess) -> BusResponse {
            match access {
                BusAccess::Read { addr, .. } if *addr == 0x1500 => BusResponse::Success(Some(0xaa)),
                BusAccess::Read { .. } => BusResponse::Success(Some(0xbb)),
                BusAccess::Write { .. } => BusResponse::Success(None),
            }
        }
        fn as_any(&self) -> &dyn Any { self }
    }

    #[test]
    fn test_multi_resource_device_mmio_and_pio() {
        let mut router = BusRouter::new();
        let dev = Arc::new(MultiResDevice {
            resources: vec![
                Resource::Mmio(0x1000..0x2000),
                Resource::Pio(0x60..0x70),
            ],
        });
        router.register(dev).unwrap();

        // MMIO access
        let resp = router.route(
            BusKind::Mmio,
            &BusAccess::Read { addr: 0x1500, width: AccessWidth::U32 },
        );
        assert!(matches!(resp, BusResponse::Success(Some(0xaa))));

        // PIO access
        let resp = router.route(
            BusKind::Pio,
            &BusAccess::Read { addr: 0x65, width: AccessWidth::U8 },
        );
        assert!(matches!(resp, BusResponse::Success(Some(0xbb))));
    }

    // ── Interrupt injection with mock controller ─────────────────────────

    #[derive(Debug)]
    struct MockIntc {
        id: DeviceId,
        resources: Vec<Resource>,
        last_injected: std::sync::Mutex<Option<(u32, TriggerMode, Option<IrqTarget>)>>,
    }

    impl MockIntc {
        fn new(id: DeviceId) -> Self {
            Self {
                id,
                resources: vec![Resource::Mmio(0xf000_0000..0xf010_0000)],
                last_injected: std::sync::Mutex::new(None),
            }
        }
    }

    impl VirtualDevice for MockIntc {
        fn id(&self) -> DeviceId { self.id }
        fn name(&self) -> &str { "mock-intc" }
        fn resources(&self) -> &[Resource] { &self.resources }
        fn handle_access(&self, _bus: BusKind, _access: &BusAccess) -> BusResponse {
            BusResponse::Success(None)
        }
        fn as_interrupt_controller(&self) -> Option<&dyn InterruptControllerOps> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any { self }
    }

    impl InterruptControllerOps for MockIntc {
        fn inject_irq(&self, pin: u32, trigger: TriggerMode, target: Option<IrqTarget>) -> Result<()> {
            *self.last_injected.lock().unwrap() = Some((pin, trigger, target));
            Ok(())
        }
        fn deactivate_irq(&self, _pin: u32) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_inject_with_controller() {
        let mut router = BusRouter::new();
        let intc_id = router.register(Arc::new(MockIntc::new(DeviceId::from_u64(0)))).unwrap();

        // Build IRQ routing table
        router.irq_table_mut().add_legacy(
            IrqLine(33), intc_id, 5, TriggerMode::Edge, None, "test-dev",
        );

        let result = router.inject(IrqMessage::leg(IrqLine(33)));
        assert!(result.is_ok());

        // Verify the controller received it by checking the test's side effect
        // We need to look up the controller to verify
        let ctrl = router.find_interrupt_controller(intc_id).unwrap();
        let mock = ctrl.as_any().downcast_ref::<MockIntc>().unwrap();
        let guard = mock.last_injected.lock().unwrap();
        let (pin, trigger, target) = guard.expect("expected injected value");
        assert_eq!(pin, 5);
        assert_eq!(trigger, TriggerMode::Edge);
        assert_eq!(target, None);
    }

    #[test]
    fn test_inject_unknown_line() {
        let mut router = BusRouter::new();
        // Register a controller but don't add a routing entry for line 33
        let _intc_id = router.register(Arc::new(MockIntc::new(DeviceId::from_u64(1)))).unwrap();

        let result = router.inject(IrqMessage::leg(IrqLine(33)));
        assert!(matches!(result, Err(DeviceError::NotFound)));
    }

    // ── Exact boundary non-conflict ──────────────────────────────────────

    #[test]
    fn test_exact_boundary_non_conflict() {
        let mut router = BusRouter::new();
        let dev_a = Arc::new(MultiResDevice {
            resources: vec![Resource::Mmio(0x1000..0x2000)],
        });
        let dev_b = Arc::new(MultiResDevice {
            resources: vec![Resource::Mmio(0x2000..0x3000)],
        });
        // Adjacent ranges [0x1000, 0x2000) and [0x2000, 0x3000) should NOT conflict
        router.register(dev_a).unwrap();
        router.register(dev_b).unwrap();
        assert_eq!(router.total_devices(), 2);
    }

    // ── Adapter translation correctness ────────────────────────────────

    /// A mock implementing BaseDeviceOps<GuestPhysAddrRange> to verify adapter translation.
    use axdevice_base::BaseDeviceOps as _;
    use std::sync::{Arc as StdArc2, Mutex as StdMtx};
    struct MockMmioDev {
        range: axaddrspace::GuestPhysAddrRange,
        last_read: StdMtx<Option<(u64, axaddrspace::device::AccessWidth)>>,
        last_write: StdMtx<Option<(u64, axaddrspace::device::AccessWidth, usize)>>,
    }
    impl axdevice_base::BaseDeviceOps<axaddrspace::GuestPhysAddrRange> for MockMmioDev {
        fn emu_type(&self) -> axdevice_base::EmuDeviceType { axdevice_base::EmuDeviceType::Dummy }
        fn address_range(&self) -> axaddrspace::GuestPhysAddrRange { self.range.clone() }
        fn handle_read(&self, addr: axaddrspace::GuestPhysAddr, width: axaddrspace::device::AccessWidth) -> ax_errno::AxResult<usize> {
            *self.last_read.lock().unwrap() = Some((addr.as_usize() as u64, width));
            Ok(0xab)
        }
        fn handle_write(&self, addr: axaddrspace::GuestPhysAddr, width: axaddrspace::device::AccessWidth, val: usize) -> ax_errno::AxResult {
            *self.last_write.lock().unwrap() = Some((addr.as_usize() as u64, width, val));
            Ok(())
        }
    }

    #[test]
    fn test_legacy_mmio_adapter_translates_read() {
        let mock = StdArc2::new(MockMmioDev {
            range: axaddrspace::GuestPhysAddrRange::from_start_size(0x1000.into(), 0x1000),
            last_read: StdMtx::new(None),
            last_write: StdMtx::new(None),
        });
        let mmio_dev: Arc<dyn axdevice_base::BaseDeviceOps<axaddrspace::GuestPhysAddrRange>> = mock.clone();
        let adapter = crate::LegacyMmioAdapter::new(DeviceId(1), mmio_dev);
        let bus_access = BusAccess::Read { addr: 0x1500, width: AccessWidth::U32 };
        let resp = adapter.handle_access(BusKind::Mmio, &bus_access);
        assert!(matches!(resp, BusResponse::Success(Some(0xab))));
        let read = mock.last_read.lock().unwrap().unwrap();
        assert_eq!(read.0, 0x1500);
        assert_eq!(read.1, axaddrspace::device::AccessWidth::Dword);
    }

    #[test]
    fn test_legacy_mmio_adapter_translates_write() {
        let mock = StdArc2::new(MockMmioDev {
            range: axaddrspace::GuestPhysAddrRange::from_start_size(0x1000.into(), 0x1000),
            last_read: StdMtx::new(None),
            last_write: StdMtx::new(None),
        });
        let mmio_dev: Arc<dyn axdevice_base::BaseDeviceOps<axaddrspace::GuestPhysAddrRange>> = mock.clone();
        let adapter = crate::LegacyMmioAdapter::new(DeviceId(2), mmio_dev);
        let bus_access = BusAccess::Write { addr: 0x1234, width: AccessWidth::U16, val: 0xabcd };
        let resp = adapter.handle_access(BusKind::Mmio, &bus_access);
        assert!(matches!(resp, BusResponse::Success(None)));
        let write = mock.last_write.lock().unwrap().unwrap();
        assert_eq!(write.0, 0x1234);
        assert_eq!(write.1, axaddrspace::device::AccessWidth::Word);
        assert_eq!(write.2, 0xabcd);
    }

    // ── SysReg end-to-end through adapter ──────────────────────────────

    use axaddrspace::device::{SysRegAddr, SysRegAddrRange};

    struct MockSysRegDev {
        range: SysRegAddrRange,
    }
    impl axdevice_base::BaseDeviceOps<SysRegAddrRange> for MockSysRegDev {
        fn emu_type(&self) -> axdevice_base::EmuDeviceType { axdevice_base::EmuDeviceType::Dummy }
        fn address_range(&self) -> SysRegAddrRange { self.range.clone() }
        fn handle_read(&self, addr: SysRegAddr, _width: axaddrspace::device::AccessWidth) -> ax_errno::AxResult<usize> {
            if addr.0 == 0x100 { Ok(0xcc) } else { Err(ax_errno::AxError::NotFound) }
        }
        fn handle_write(&self, addr: SysRegAddr, _width: axaddrspace::device::AccessWidth, _val: usize) -> ax_errno::AxResult {
            if addr.0 == 0x100 { Ok(()) } else { Err(ax_errno::AxError::NotFound) }
        }
    }

    #[test]
    fn test_sysreg_adapter_route_end_to_end() {
        let inner: Arc<dyn axdevice_base::BaseDeviceOps<SysRegAddrRange>> =
            Arc::new(MockSysRegDev {
                range: SysRegAddrRange::new(SysRegAddr(0x100), SysRegAddr(0x200)),
            });
        let adapter = crate::LegacySysRegAdapter::new(DeviceId(3), inner);
        let mut router = BusRouter::new();
        router.register(Arc::new(adapter)).unwrap();

        let resp = router.route(BusKind::SysReg, &BusAccess::Read { addr: 0x100, width: AccessWidth::U64 });
        assert!(matches!(resp, BusResponse::Success(Some(0xcc))));

        let resp_miss = router.route(BusKind::SysReg, &BusAccess::Read { addr: 0x999, width: AccessWidth::U64 });
        // The adapter returns InvalidAccess because the mock device returns
        // Err for addresses outside its handled range — the adapter can't
        // distinguish "no device" from "device error" through the AxResult.
        assert!(matches!(resp_miss, BusResponse::InvalidAccess));
    }

    // ── MSI positive test ──────────────────────────────────────────────

    #[derive(Debug)]
    struct MockMsiIntc {
        id: DeviceId,
        last_msi: std::sync::Mutex<Option<(u64, u32)>>,
    }
    impl VirtualDevice for MockMsiIntc {
        fn id(&self) -> DeviceId { self.id }
        fn name(&self) -> &str { "mock-msi-intc" }
        fn resources(&self) -> &[Resource] { &[Resource::Mmio(0xf000_0000..0xf010_0000)] }
        fn handle_access(&self, _bus: BusKind, _access: &BusAccess) -> BusResponse { BusResponse::Success(None) }
        fn as_interrupt_controller(&self) -> Option<&dyn InterruptControllerOps> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any { self }
    }
    impl InterruptControllerOps for MockMsiIntc {
        fn inject_irq(&self, _pin: u32, _trigger: TriggerMode, _target: Option<IrqTarget>) -> Result<()> { Err(DeviceError::NotFound) }
        fn deactivate_irq(&self, _pin: u32) -> Result<()> { Err(DeviceError::NotFound) }
        fn handle_msi(&self, addr: u64, data: u32) -> Result<()> {
            *self.last_msi.lock().unwrap() = Some((addr, data));
            Ok(())
        }
    }

    #[test]
    fn test_msi_inject_with_controller() {
        let mut router = BusRouter::new();
        let intc_id = router.register(Arc::new(MockMsiIntc {
            id: DeviceId(100),
            last_msi: std::sync::Mutex::new(None),
        })).unwrap();
        router.irq_table_mut().add_msi_range(0xfee0_0000..0xfee1_0000, intc_id);

        let result = router.inject(IrqMessage::msi(0xfee0_1234, 0x42));
        assert!(result.is_ok());
    }

    // ── Factory + Router end-to-end ──────────────────────────────────

    #[test]
    fn test_factory_to_router_e2e() {
        use axvmconfig::EmulatedDeviceConfig;

        struct E2eFactory;
        impl DeviceFactory for E2eFactory {
            fn emu_type(&self) -> EmulatedDeviceType { EmulatedDeviceType::Dummy }
            fn create(&self, _cfg: &EmulatedDeviceConfig, id_gen: &mut dyn FnMut() -> DeviceId) -> Result<Box<dyn VirtualDevice>> {
                let id = id_gen();
                #[derive(Debug)]
                struct E2eDev(DeviceId);
                impl VirtualDevice for E2eDev {
                    fn id(&self) -> DeviceId { self.0 }
                    fn name(&self) -> &str { "e2e" }
                    fn resources(&self) -> &[Resource] { &[Resource::Mmio(0x5000..0x6000)] }
                    fn handle_access(&self, _b: BusKind, a: &BusAccess) -> BusResponse {
                        match a { BusAccess::Read { .. } => BusResponse::Success(Some(0xee)), _ => BusResponse::Success(None) }
                    }
                    fn as_any(&self) -> &dyn Any { self }
                }
                Ok(Box::new(E2eDev(id)))
            }
        }

        let mut factories = crate::FactoryRegistry::new();
        factories.register(Box::new(E2eFactory));

        let configs = alloc::vec![EmulatedDeviceConfig { emu_type: EmulatedDeviceType::Dummy, ..Default::default() }];
        let mut counter = 0u64;
        let mut id_gen = || { counter += 1; DeviceId(counter) };

        let mut router = BusRouter::new();
        for result in factories.create_all(&configs, &mut id_gen) {
            router.register(Arc::from(result.unwrap())).unwrap();
        }
        assert_eq!(router.total_devices(), 1);

        let resp = router.route(BusKind::Mmio, &BusAccess::Read { addr: 0x5500, width: AccessWidth::U32 });
        assert!(matches!(resp, BusResponse::Success(Some(0xee))));
    }
}
