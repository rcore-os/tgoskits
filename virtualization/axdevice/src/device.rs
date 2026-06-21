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

use alloc::{format, rc::Rc, sync::Arc, vec, vec::Vec};
use core::ops::Range;

#[cfg(target_arch = "aarch64")]
use arm_vgic::Vgic;
use ax_errno::{AxError, AxResult, ax_err, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(target_arch = "aarch64")]
use ax_memory_addr::PhysAddr;
use ax_memory_addr::is_aligned_4k;
use axdevice_base::{
    AccessWidth, BaseDeviceOps, BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps,
    DeviceAddrRange, Port, PortRange, SysRegAddr, SysRegAddrRange,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange};
#[cfg(target_arch = "riscv64")]
use riscv_vplic::VPlicGlobal;
#[cfg(target_arch = "x86_64")]
use x86_vlapic::{EmulatedIoApic, EmulatedPit, EmulatedSerialPort, IoApicInterrupt};

use crate::{
    AxVmDeviceConfig, BusAccess, BusResponse, DeviceBuildContext, DeviceCapabilities, DeviceError,
    DeviceId, DeviceOps, DeviceRegistry, DeviceResult, LegacyDeviceAdapter, Resource,
    range_alloc::RangeAllocator,
};

/// A set of emulated device types that can be accessed by a specific address range type.
pub struct AxEmuDevices<R: DeviceAddrRange> {
    emu_devices: Vec<Arc<dyn BaseDeviceOps<R>>>,
}

impl<R: DeviceAddrRange + 'static> AxEmuDevices<R> {
    /// Creates a new [`AxEmuDevices`] instance.
    pub fn new() -> Self {
        Self {
            emu_devices: Vec::new(),
        }
    }

    /// Adds a device to the set.
    pub fn add_dev(&mut self, dev: Arc<dyn BaseDeviceOps<R>>) {
        self.emu_devices.push(dev);
    }

    // pub fn remove_dev(&mut self, ...)
    //
    // `remove_dev` seems to need something like `downcast-rs` to make sense. As it's not likely to
    // be able to have a proper predicate to remove a device from the list without knowing the
    // concrete type of the device.

    /// Find a device by address.
    pub fn find_dev(&self, addr: R::Addr) -> Option<Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices
            .iter()
            .find(|&dev| dev.address_range().contains(addr))
            .cloned()
    }

    /// Iterates over the devices in the set.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter()
    }

    /// Iterates over the devices in the set mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseDeviceOps<R>>> {
        self.emu_devices.iter_mut()
    }
}

impl<R: DeviceAddrRange + 'static> Default for AxEmuDevices<R> {
    fn default() -> Self {
        Self::new()
    }
}

type AxEmuMmioDevices = AxEmuDevices<GuestPhysAddrRange>;
type AxEmuSysRegDevices = AxEmuDevices<SysRegAddrRange>;
type AxEmuPortDevices = AxEmuDevices<PortRange>;

/// represent A vm own devices
pub struct AxVmDevices {
    /// emu devices
    emu_mmio_devices: AxEmuMmioDevices,
    emu_sys_reg_devices: AxEmuSysRegDevices,
    emu_port_devices: AxEmuPortDevices,
    registry: DeviceRegistry,
    next_device_id: usize,
    #[cfg(target_arch = "x86_64")]
    x86_ioapic: Option<Arc<EmulatedIoApic>>,
    #[cfg(target_arch = "x86_64")]
    x86_pit: Option<Arc<EmulatedPit>>,
    #[cfg(target_arch = "x86_64")]
    x86_serial: Option<Arc<EmulatedSerialPort>>,
    /// IVC channel range allocator
    ivc_channel: Option<Mutex<RangeAllocator>>,
}

fn device_error_to_ax_error(error: DeviceError) -> AxError {
    match error {
        DeviceError::Backend(error) => error,
        DeviceError::DeviceNotFound { .. } => {
            ax_err_type!(NotFound, format!("device dispatch failed: {error}"))
        }
        DeviceError::InvalidAccessWidth { .. }
        | DeviceError::BusAddressMismatch { .. }
        | DeviceError::AddressOutOfRange { .. } => {
            ax_err_type!(InvalidInput, format!("device dispatch failed: {error}"))
        }
        DeviceError::UnsupportedOperation => {
            ax_err_type!(Unsupported, format!("device dispatch failed: {error}"))
        }
        DeviceError::ReadOnly { .. } | DeviceError::WriteOnly { .. } => {
            ax_err_type!(PermissionDenied, format!("device dispatch failed: {error}"))
        }
        DeviceError::DuplicateDeviceId { .. } | DeviceError::ResourceConflict { .. } => {
            ax_err_type!(BadState, format!("device dispatch failed: {error}"))
        }
    }
}

fn unexpected_device_response(access: BusAccess, response: BusResponse) -> AxError {
    ax_err_type!(
        BadState,
        format!("unexpected device response {response:?} for access {access:?}")
    )
}

fn dispatch_device_read(devices: &AxVmDevices, access: BusAccess) -> AxResult<usize> {
    trace!("emu_device read: {access:?}");

    match devices
        .dispatch_bus_access(access)
        .map_err(device_error_to_ax_error)?
    {
        BusResponse::Read { value } => Ok(value),
        response => Err(unexpected_device_response(access, response)),
    }
}

fn dispatch_device_write(devices: &AxVmDevices, access: BusAccess) -> AxResult {
    trace!("emu_device write: {access:?}");

    match devices
        .dispatch_bus_access(access)
        .map_err(device_error_to_ax_error)?
    {
        BusResponse::Write => Ok(()),
        response => Err(unexpected_device_response(access, response)),
    }
}

impl DeviceBuildContext for AxVmDevices {
    fn alloc_device_id(&mut self) -> DeviceId {
        Self::alloc_device_id(self)
    }
}

#[cfg(target_arch = "aarch64")]
fn init_from_aarch64_catalog(
    devices: &mut AxVmDevices,
    config: &EmulatedDeviceConfig,
) -> DeviceResult<bool> {
    let catalog = crate::DeviceFactoryCatalog::from_linker()?;

    let Some(factory) = catalog.find_unique(config.emu_type)? else {
        return Ok(false);
    };

    info!(
        "aarch64 linker device factory matched: type={:?}, name={}, base_gpa={:#x}, length={:#x}",
        config.emu_type, config.name, config.base_gpa, config.length
    );

    let built_devices = factory.build(devices, config)?;
    let built_count = built_devices.len();
    for device in &built_devices {
        info!(
            "aarch64 device factory built native device: id={:?}, name={}, resources={:?}",
            device.id(),
            device.name(),
            device.resources()
        );
    }

    devices.register_factory_devices(built_devices)?;
    info!(
        "aarch64 device factory registered {built_count} native device(s) for type {:?}",
        config.emu_type
    );
    Ok(true)
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> Self {
        let mut this = Self {
            emu_mmio_devices: AxEmuMmioDevices::new(),
            emu_sys_reg_devices: AxEmuSysRegDevices::new(),
            emu_port_devices: AxEmuPortDevices::new(),
            registry: DeviceRegistry::new(),
            next_device_id: 0,
            #[cfg(target_arch = "x86_64")]
            x86_ioapic: None,
            #[cfg(target_arch = "x86_64")]
            x86_pit: None,
            #[cfg(target_arch = "x86_64")]
            x86_serial: None,
            ivc_channel: None,
        };

        Self::init(&mut this, &config.emu_configs);
        this
    }

    /// According the emu_configs to init every  specific device
    fn init(this: &mut Self, emu_configs: &Vec<EmulatedDeviceConfig>) {
        for config in emu_configs {
            #[cfg(target_arch = "aarch64")]
            match init_from_aarch64_catalog(this, config) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(err) => {
                    panic!(
                        "failed to initialize emulated device {} ({:?}): {err}",
                        config.name, config.emu_type
                    );
                }
            }

            match config.emu_type {
                EmulatedDeviceType::InterruptController => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        this.add_mmio_dev(Arc::new(Vgic::new()));
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTRedistributor => {
                    warn!(
                        "emu type: {} is not supported by the active device factory catalog",
                        config.emu_type
                    );
                }
                EmulatedDeviceType::GPPTDistributor => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        #[allow(clippy::arc_with_non_send_sync)]
                        this.add_mmio_dev(Arc::new(arm_vgic::v3::vgicd::VGicD::new(
                            config.base_gpa.into(),
                            Some(config.length),
                        )));

                        info!(
                            "GPPT Distributor initialized with base GPA {base_gpa:#x} and length \
                             {length:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length
                        );
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTITS => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        let host_gits_base = config
                            .cfg_list
                            .first()
                            .copied()
                            .map(PhysAddr::from_usize)
                            .expect("expect 1 arg for gppt its (host_gits_base)");

                        #[allow(clippy::arc_with_non_send_sync)]
                        this.add_mmio_dev(Arc::new(arm_vgic::v3::gits::Gits::new(
                            config.base_gpa.into(),
                            Some(config.length),
                            host_gits_base,
                            false,
                        )));

                        info!(
                            "GPPT ITS initialized with base GPA {base_gpa:#x} and length \
                             {length:#x}, host GITS base {host_gits_base:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length,
                            host_gits_base = host_gits_base
                        );
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::PPPTGlobal => {
                    #[cfg(target_arch = "riscv64")]
                    {
                        let context_num = config
                            .cfg_list
                            .first()
                            .copied()
                            .expect("expect 1 arg for pppt global (context_num)");
                        this.add_mmio_dev(Arc::new(VPlicGlobal::new(
                            config.base_gpa.into(),
                            Some(config.length),
                            context_num, // Here only 1 core and should be cpu0
                        )));
                        // PLIC Partial Passthrough Global.
                        info!(
                            "Partial PLIC Passthrough Global initialized with base GPA {:#x} and \
                             length {:#x}",
                            config.base_gpa, config.length
                        );
                    }
                    #[cfg(not(target_arch = "riscv64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::Console => {
                    #[cfg(target_arch = "x86_64")]
                    {
                        let serial = Arc::new(EmulatedSerialPort::new());
                        this.x86_serial = Some(Arc::clone(&serial));
                        this.add_port_dev(serial);
                        info!("x86 16550 serial initialized for ports 0x3f8..=0x3ff");
                    }
                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::X86IoApic => {
                    #[cfg(target_arch = "x86_64")]
                    {
                        let ioapic = Arc::new(EmulatedIoApic::new(
                            config.base_gpa.into(),
                            Some(config.length),
                        ));
                        this.x86_ioapic = Some(Arc::clone(&ioapic));
                        this.add_mmio_dev(ioapic);
                        info!(
                            "x86 IO APIC initialized with base GPA {:#x} and length {:#x}",
                            config.base_gpa, config.length
                        );
                    }
                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::X86Pit => {
                    #[cfg(target_arch = "x86_64")]
                    {
                        let pit = Arc::new(EmulatedPit::new());
                        this.x86_pit = Some(Arc::clone(&pit));
                        this.add_port_dev(pit);
                        info!("x86 PIT initialized for ports 0x40..=0x43 and 0x61");
                    }
                    #[cfg(not(target_arch = "x86_64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::IVCChannel => {
                    if this.ivc_channel.is_none() {
                        // Initialize the IVC channel range allocator
                        this.ivc_channel = Some(Mutex::new(RangeAllocator::new(Range {
                            start: config.base_gpa,
                            end: config.base_gpa + config.length,
                        })));
                        info!(
                            "IVCChannel initialized with base GPA {base_gpa:#x} and length \
                             {length:#x}",
                            base_gpa = config.base_gpa,
                            length = config.length
                        );
                    } else {
                        warn!("IVCChannel already initialized, ignoring additional config");
                    }
                }
                _ => {
                    warn!(
                        "Emulated device {}'s type {:?} is not supported yet",
                        config.name, config.emu_type
                    );
                }
            }
        }
    }

    /// Allocates an IVC (Inter-VM Communication) channel of the specified size.
    pub fn alloc_ivc_channel(&self, size: usize) -> AxResult<GuestPhysAddr> {
        if size == 0 {
            return ax_err!(InvalidInput, "Size must be greater than 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "Size must be aligned to 4K");
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .allocate_range(size)
                .ok_or_else(|| {
                    warn!("Failed to allocate IVC channel range with size {size:#x}");
                    ax_errno::ax_err_type!(NoMemory, "IVC channel allocation failed")
                })
                .map(|range| {
                    debug!("Allocated IVC channel range: {range:x?}");
                    GuestPhysAddr::from_usize(range.start)
                })
        } else {
            ax_err!(InvalidInput, "IVC channel not exists")
        }
    }

    /// Releases an IVC channel at the specified address and size.
    pub fn release_ivc_channel(&self, addr: GuestPhysAddr, size: usize) -> AxResult {
        if size == 0 {
            return ax_err!(InvalidInput, "Size must be greater than 0");
        }
        if !is_aligned_4k(size) {
            return ax_err!(InvalidInput, "Size must be aligned to 4K");
        }

        if let Some(allocator) = &self.ivc_channel {
            let range = addr.as_usize()..addr.as_usize() + size;
            if allocator.lock().free_range(range.clone()) {
                debug!("Released IVC channel range: {range:x?}");
                Ok(())
            } else {
                ax_err!(InvalidInput, "Invalid IVC channel range")
            }
        } else {
            ax_err!(InvalidInput, "IVC channel not exists")
        }
    }

    fn alloc_device_id(&mut self) -> DeviceId {
        let id = DeviceId::new(self.next_device_id);
        self.next_device_id += 1;
        id
    }

    fn register_legacy_device(&mut self, adapter: LegacyDeviceAdapter) {
        if let Err(err) = self.registry.register_device(Rc::new(adapter)) {
            panic!("failed to register legacy device in DeviceRegistry: {err}");
        }
    }

    /// Registers a native device directly in the device registry.
    ///
    /// Unlike the legacy `add_*_dev` helpers, this does not add the device to
    /// the old MMIO/PIO/SysReg lists. The device must declare its own bus
    /// resources through [`DeviceOps::resources`].
    pub fn register_device(&mut self, device: Rc<dyn DeviceOps>) -> DeviceResult<DeviceId> {
        self.registry.register_device(device)
    }

    #[cfg(target_arch = "aarch64")]
    fn register_factory_devices(&mut self, devices: Vec<Rc<dyn DeviceOps>>) -> DeviceResult {
        for device in devices {
            self.register_device(device)?;
        }
        Ok(())
    }

    /// Add a MMIO device to the device list.
    pub fn add_mmio_dev(&mut self, dev: Arc<dyn BaseMmioDeviceOps>) {
        let id = self.alloc_device_id();
        let name = format!("legacy-mmio-{}", dev.emu_type());
        let resources = vec![Resource::Mmio(dev.address_range())];
        self.register_legacy_device(LegacyDeviceAdapter::mmio(
            id,
            name,
            resources,
            DeviceCapabilities::none(),
            Arc::clone(&dev),
        ));
        self.emu_mmio_devices.add_dev(dev);
    }

    /// Add a system register device to the device list.
    pub fn add_sys_reg_dev(&mut self, dev: Arc<dyn BaseSysRegDeviceOps>) {
        let id = self.alloc_device_id();
        let name = format!("legacy-sysreg-{}", dev.emu_type());
        let resources = vec![Resource::SysReg(dev.address_range())];
        self.register_legacy_device(LegacyDeviceAdapter::sysreg(
            id,
            name,
            resources,
            DeviceCapabilities::none(),
            Arc::clone(&dev),
        ));
        self.emu_sys_reg_devices.add_dev(dev);
    }

    /// Add a port device to the device list.
    pub fn add_port_dev(&mut self, dev: Arc<dyn BasePortDeviceOps>) {
        let id = self.alloc_device_id();
        let name = format!("legacy-pio-{}", dev.emu_type());
        let resources = vec![Resource::Pio(dev.address_range())];
        self.register_legacy_device(LegacyDeviceAdapter::pio(
            id,
            name,
            resources,
            DeviceCapabilities::none(),
            Arc::clone(&dev),
        ));
        self.emu_port_devices.add_dev(dev);
    }

    /// Dispatches a normalized bus access through the new device registry.
    pub fn dispatch_bus_access(&self, access: BusAccess) -> DeviceResult<BusResponse> {
        self.registry.dispatch(access)
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mmio_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_sys_reg_dev(&self) -> impl Iterator<Item = &Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_port_dev(&self) -> impl Iterator<Item = &Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter()
    }

    /// Returns the guest vector programmed for an x86 IOAPIC GSI.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.x86_ioapic
            .as_ref()
            .and_then(|ioapic| ioapic.vector_for_gsi(gsi))
    }

    /// Assert an x86 IOAPIC GSI and return the interrupt to inject.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_assert_gsi(&self, gsi: usize) -> Option<IoApicInterrupt> {
        self.x86_ioapic
            .as_ref()
            .and_then(|ioapic| ioapic.assert_gsi(gsi))
    }

    /// Broadcast an x86 local APIC EOI to the virtual IOAPIC.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_end_of_interrupt(&self, vector: u8) -> Option<IoApicInterrupt> {
        self.x86_ioapic
            .as_ref()
            .and_then(|ioapic| ioapic.end_of_interrupt(vector))
    }

    /// Consume a pending x86 PIT channel 0 timer tick if the deadline is due.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_pit_consume_irq0_if_due(&self, now_ns: u64) -> bool {
        self.x86_pit
            .as_ref()
            .is_some_and(|pit| pit.consume_irq0_if_due(now_ns))
    }

    /// Poll x86 COM1 and return whether it has a pending RX interrupt.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_serial_poll_irq(&self) -> bool {
        self.x86_serial
            .as_ref()
            .is_some_and(|serial| serial.poll_irq())
    }

    /// Iterates over the MMIO devices in the set.
    pub fn iter_mut_mmio_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.iter_mut()
    }

    /// Iterates over the system register devices in the set.
    pub fn iter_mut_sys_reg_dev(
        &mut self,
    ) -> impl Iterator<Item = &mut Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.iter_mut()
    }

    /// Iterates over the port devices in the set.
    pub fn iter_mut_port_dev(&mut self) -> impl Iterator<Item = &mut Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.iter_mut()
    }

    /// Find specific MMIO device by ipa
    pub fn find_mmio_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn BaseMmioDeviceOps>> {
        self.emu_mmio_devices.find_dev(ipa)
    }

    /// Find specific system register device by ipa
    pub fn find_sys_reg_dev(
        &self,
        sys_reg_addr: SysRegAddr,
    ) -> Option<Arc<dyn BaseSysRegDeviceOps>> {
        self.emu_sys_reg_devices.find_dev(sys_reg_addr)
    }

    /// Find specific port device by port number
    pub fn find_port_dev(&self, port: Port) -> Option<Arc<dyn BasePortDeviceOps>> {
        self.emu_port_devices.find_dev(port)
    }

    /// Handle the MMIO read by GuestPhysAddr and data width, return the value of the guest want to read
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        dispatch_device_read(self, BusAccess::mmio_read(addr, width))
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        dispatch_device_write(self, BusAccess::mmio_write(addr, width, val))
    }

    /// Handle the system register read by SysRegAddr and data width, return the value of the guest want to read
    pub fn handle_sys_reg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        dispatch_device_read(self, BusAccess::sysreg_read(addr, width))
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write, call specific device to write the value
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        dispatch_device_write(self, BusAccess::sysreg_write(addr, width, val))
    }

    /// Handle the port read by port number and data width, return the value of the guest want to read
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        dispatch_device_read(self, BusAccess::pio_read(port, width))
    }

    /// Handle the port write by port number, data width and the value need to write, call specific device to write the value
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        dispatch_device_write(self, BusAccess::pio_write(port, width, val))
    }
}
