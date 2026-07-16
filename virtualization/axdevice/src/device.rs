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

use alloc::{collections::BTreeMap, format, sync::Arc, vec::Vec};
use core::ops::Range;

use ax_kspin::SpinNoIrq as Mutex;
use ax_memory_addr::is_aligned_4k;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, BusRouter, Device, DeviceError, DeviceId,
    DeviceRegistry, InvalidResourceReason, MmioDeviceAdapter, Port, RegistryError, Resource,
    SysRegAddr,
};
use axvm_types::GuestPhysAddr;
#[cfg(target_arch = "x86_64")]
use x86_vlapic::IoApicEoi;

#[cfg(any(target_arch = "loongarch64", target_arch = "x86_64"))]
use crate::DeviceRegistration;
#[cfg(target_arch = "loongarch64")]
use crate::LoongArchPchPicRuntimeOps;
use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, FwCfg, InterruptTopology,
    PollableDeviceOps, range_alloc::RangeAllocator,
};
#[cfg(target_arch = "x86_64")]
use crate::{X86IoApicRuntimeOps, X86PitDeviceOps, X86SerialDeviceOps};

#[inline]
#[allow(dead_code)]
fn log_device_io(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    addr_range: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) {
    let rw = if read { "read" } else { "write" };
    trace!("emu_device {rw}: {addr_type} {addr:#x} in range {addr_range:#x} with width {width:?}")
}

/// Internal range entry cached in the index maps.
struct RangeEntry {
    slot: usize,
    size: u64,
}

/// represent A vm own devices
pub struct AxVmDevices {
    /// Registered devices (append-only; index is the DeviceId).
    devices: Vec<Arc<dyn Device>>,
    /// MMIO base address → range entry (slot, size).
    mmio_index: BTreeMap<u64, RangeEntry>,
    /// Port I/O base address → range entry (slot, size).
    port_index: BTreeMap<u16, RangeEntry>,
    /// System register address → range entry (slot, count).
    sysreg_index: BTreeMap<u32, RangeEntry>,
    /// Devices that require periodic polling.
    pollable_devices: Vec<Arc<dyn PollableDeviceOps>>,
    /// x86 IOAPIC — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_ioapic: Option<Arc<dyn X86IoApicRuntimeOps>>,
    /// x86 PIT — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_pit: Option<Arc<dyn X86PitDeviceOps>>,
    /// x86 16550 serial port — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_serial: Option<Arc<dyn X86SerialDeviceOps>>,
    /// LoongArch PCH-PIC — kept for type-specific access.
    #[cfg(target_arch = "loongarch64")]
    loongarch_pch_pic: Option<Arc<dyn LoongArchPchPicRuntimeOps>>,
    /// QEMU fw_cfg — kept for DMA access routing.
    fw_cfg: Option<Arc<FwCfg>>,
    /// IVC channel range allocator
    ivc_channel: Option<Mutex<RangeAllocator>>,
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    /// Creates an empty VM device registry for staged platform initialization.
    pub fn empty() -> Self {
        Self {
            devices: Vec::new(),
            mmio_index: BTreeMap::new(),
            port_index: BTreeMap::new(),
            sysreg_index: BTreeMap::new(),
            pollable_devices: Vec::new(),
            #[cfg(target_arch = "x86_64")]
            x86_ioapic: None,
            #[cfg(target_arch = "x86_64")]
            x86_pit: None,
            #[cfg(target_arch = "x86_64")]
            x86_serial: None,
            #[cfg(target_arch = "loongarch64")]
            loongarch_pch_pic: None,
            fw_cfg: None,
            ivc_channel: None,
        }
    }

    /// Configures the guest-address pool used for inter-VM communication.
    pub fn configure_ivc_range(&mut self, base: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        if size == 0 || !is_aligned_4k(base.as_usize()) || !is_aligned_4k(size) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "configure IVC address range",
                detail: format!(
                    "range [{:#x}, +{size:#x}) must be non-empty and 4 KiB aligned",
                    base.as_usize()
                ),
            });
        }
        let end =
            base.as_usize()
                .checked_add(size)
                .ok_or_else(|| DeviceManagerError::InvalidInput {
                    operation: "configure IVC address range",
                    detail: "address range overflows usize".into(),
                })?;
        if self.ivc_channel.is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "configure IVC address range",
                detail: "an IVC address range is already configured".into(),
            });
        }
        self.ivc_channel = Some(Mutex::new(RangeAllocator::new(Range {
            start: base.as_usize(),
            end,
        })));
        Ok(())
    }

    /// Allocates an IVC (Inter-VM Communication) channel of the specified size.
    pub fn alloc_ivc_channel(&self, size: usize) -> DeviceManagerResult<GuestPhysAddr> {
        if size == 0 {
            return Err(DeviceManagerError::InvalidInput {
                operation: "allocate IVC channel",
                detail: "size must be greater than zero".into(),
            });
        }
        if !is_aligned_4k(size) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "allocate IVC channel",
                detail: format!("size {size:#x} is not aligned to 4 KiB"),
            });
        }

        if let Some(allocator) = &self.ivc_channel {
            allocator
                .lock()
                .allocate_range(size)
                .ok_or_else(|| {
                    warn!("Failed to allocate IVC channel range with size {size:#x}");
                    DeviceManagerError::OutOfMemory {
                        operation: "allocate IVC channel",
                    }
                })
                .map(|range| {
                    debug!("Allocated IVC channel range: {range:x?}");
                    GuestPhysAddr::from_usize(range.start)
                })
        } else {
            Err(DeviceManagerError::ResourceNotFound {
                operation: "allocate IVC channel",
                resource: "IVC channel allocator".into(),
            })
        }
    }

    /// Releases an IVC channel at the specified address and size.
    pub fn release_ivc_channel(&self, addr: GuestPhysAddr, size: usize) -> DeviceManagerResult {
        if size == 0 {
            return Err(DeviceManagerError::InvalidInput {
                operation: "release IVC channel",
                detail: "size must be greater than zero".into(),
            });
        }
        if !is_aligned_4k(size) {
            return Err(DeviceManagerError::InvalidInput {
                operation: "release IVC channel",
                detail: format!("size {size:#x} is not aligned to 4 KiB"),
            });
        }

        if let Some(allocator) = &self.ivc_channel {
            let range = addr.as_usize()..addr.as_usize() + size;
            if allocator.lock().free_range(range.clone()) {
                debug!("Released IVC channel range: {range:x?}");
                Ok(())
            } else {
                Err(DeviceManagerError::InvalidInput {
                    operation: "release IVC channel",
                    detail: format!("range {range:x?} is not allocated"),
                })
            }
        } else {
            Err(DeviceManagerError::ResourceNotFound {
                operation: "release IVC channel",
                resource: "IVC channel allocator".into(),
            })
        }
    }

    /// Registers a bundle containing only device-local capabilities.
    ///
    /// Use [`Self::register_bundle_with_topology`] when the bundle contains an
    /// interrupt-controller registration.
    pub fn register_bundle(&mut self, bundle: DeviceBundle) -> DeviceManagerResult {
        if !bundle.interrupt_controllers.is_empty() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "register device bundle",
                detail: "interrupt controllers require an interrupt topology".into(),
            });
        }
        self.register_bundle_inner(bundle, None)
    }

    /// Registers device and interrupt-controller capabilities atomically.
    pub fn register_bundle_with_topology(
        &mut self,
        bundle: DeviceBundle,
        interrupt_topology: &InterruptTopology,
    ) -> DeviceManagerResult {
        self.register_bundle_inner(bundle, Some(interrupt_topology))
    }

    fn register_bundle_inner(
        &mut self,
        bundle: DeviceBundle,
        interrupt_topology: Option<&InterruptTopology>,
    ) -> DeviceManagerResult {
        for (index, pollable) in bundle.pollable.iter().enumerate() {
            if self
                .pollable_devices
                .iter()
                .chain(bundle.pollable[..index].iter())
                .any(|existing| Arc::ptr_eq(existing, pollable))
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register pollable device",
                    detail: "the same pollable capability is already registered".into(),
                });
            }
        }

        if !bundle.interrupt_controllers.is_empty() && interrupt_topology.is_none() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "register device bundle",
                detail: "interrupt controllers require an interrupt topology".into(),
            });
        }

        let mut registered_controllers = Vec::new();
        if let Some(topology) = interrupt_topology {
            for controller in bundle.interrupt_controllers {
                let id = controller.id();
                if let Err(error) = topology.register_controller(controller) {
                    Self::rollback_controllers(topology, &registered_controllers);
                    return Err(error);
                }
                registered_controllers.push(id);
            }
        }

        let saved_len = self.devices.len();
        for device in &bundle.devices {
            match self.register(device.clone()) {
                Ok(_id) => {}
                Err(e) => {
                    self.rollback_devices(saved_len);
                    if let Some(topology) = interrupt_topology {
                        Self::rollback_controllers(topology, &registered_controllers);
                    }
                    return Err(e.into());
                }
            }
        }
        self.pollable_devices.extend(bundle.pollable);
        Ok(())
    }

    fn rollback_devices(&mut self, saved_len: usize) {
        while self.devices.len() > saved_len {
            let Some(device) = self.devices.pop() else {
                break;
            };
            for resource in device.resources() {
                match *resource {
                    Resource::MmioRange { base, .. } => {
                        self.mmio_index.remove(&base);
                    }
                    Resource::PortRange { base, .. } => {
                        self.port_index.remove(&base);
                    }
                    Resource::SysReg { addr, .. } => {
                        self.sysreg_index.remove(&addr);
                    }
                }
            }
        }
    }

    fn rollback_controllers(
        topology: &InterruptTopology,
        controllers: &[axdevice_base::InterruptControllerId],
    ) {
        for controller in controllers.iter().rev() {
            if let Err(error) = topology.unregister_controller(*controller) {
                error!("failed to roll back interrupt controller {controller:?}: {error}");
            }
        }
    }

    // ─── Resource rollback ────────────────────────────────────────

    /// Removes `resources` from the index maps.  Used to undo a
    /// partially-completed insertion when a conflict is discovered
    /// mid-way through `insert_resources`.
    fn rollback_resources(&mut self, resources: &[Resource]) {
        for r in resources {
            match *r {
                Resource::MmioRange { base, .. } => {
                    self.mmio_index.remove(&base);
                }
                Resource::PortRange { base, .. } => {
                    self.port_index.remove(&base);
                }
                Resource::SysReg { addr, .. } => {
                    self.sysreg_index.remove(&addr);
                }
            }
        }
    }

    // ─── BTreeMap insertion with inline conflict detection ─────────

    /// Inserts every resource of device `idx` into the three BTreeMap
    /// indices, checking for validity errors and range conflicts
    /// as each key is inserted.
    ///
    /// Because earlier resources of the *same* device are already in
    /// the index when later ones are checked, same-device internal
    /// overlaps are caught by the same predecessor/successor probes
    /// that catch cross-device overlaps.  A conflict is reported as
    /// [`InvalidResourceReason::OverlappingResources`] when the
    /// neighbour entry belongs to the current device, and as
    /// [`RegistryError::AddressConflict`] otherwise.
    ///
    /// On any error the keys inserted so far are rolled back through
    /// [`rollback_resources`], leaving the indices unchanged.
    fn insert_resources(
        &mut self,
        idx: usize,
        resources: &[Resource],
    ) -> Result<(), RegistryError> {
        validate_resources(resources)?;
        for (i, r) in resources.iter().enumerate() {
            match *r {
                Resource::MmioRange { base, size } => {
                    // Key collision.
                    if let Some(existing) = self.mmio_index.get(&base) {
                        let existing_size = existing.size;
                        let existing_slot = existing.slot;
                        self.rollback_resources(&resources[..i]);
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::MmioRange { base, size },
                            existing: Resource::MmioRange {
                                base,
                                size: existing_size,
                            },
                            existing_device: DeviceId::new(existing_slot as u32),
                        });
                    }

                    self.mmio_index.insert(base, RangeEntry { slot: idx, size });

                    // Predecessor check.
                    if let Some((prev_base, existing)) = self.mmio_index.range(..base).next_back()
                        && prev_base.wrapping_add(existing.size) > base
                    {
                        let conflicting_base = *prev_base;
                        let conflicting_size = existing.size;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::MmioRange { base, size },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::MmioRange { base, size },
                            existing: Resource::MmioRange {
                                base: conflicting_base,
                                size: conflicting_size,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }

                    // Successor check.
                    let end = base + size;
                    if let Some(next_start) = base.checked_add(1)
                        && let Some((next_base, existing)) =
                            self.mmio_index.range(next_start..).next()
                        && *next_base < end
                    {
                        let conflicting_base = *next_base;
                        let conflicting_size = existing.size;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::MmioRange { base, size },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::MmioRange { base, size },
                            existing: Resource::MmioRange {
                                base: conflicting_base,
                                size: conflicting_size,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }
                }
                Resource::PortRange { base, size } => {
                    let end = base as u32 + size as u32;

                    // Key collision.
                    if let Some(existing) = self.port_index.get(&base) {
                        let existing_size = existing.size as u16;
                        let existing_slot = existing.slot;
                        self.rollback_resources(&resources[..i]);
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::PortRange { base, size },
                            existing: Resource::PortRange {
                                base,
                                size: existing_size,
                            },
                            existing_device: DeviceId::new(existing_slot as u32),
                        });
                    }

                    self.port_index.insert(
                        base,
                        RangeEntry {
                            slot: idx,
                            size: size as u64,
                        },
                    );

                    // Predecessor check.
                    if let Some((prev_base, existing)) = self.port_index.range(..base).next_back()
                        && (*prev_base as u32).wrapping_add(existing.size as u32) > base as u32
                    {
                        let conflicting_base = *prev_base;
                        let conflicting_size = existing.size as u16;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::PortRange { base, size },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::PortRange { base, size },
                            existing: Resource::PortRange {
                                base: conflicting_base,
                                size: conflicting_size,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }

                    // Successor check.
                    if let Some(next_port) = base.checked_add(1)
                        && let Some((next_base, existing)) =
                            self.port_index.range(next_port..).next()
                        && (*next_base as u32) < end
                    {
                        let conflicting_base = *next_base;
                        let conflicting_size = existing.size as u16;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::PortRange { base, size },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::PortRange { base, size },
                            existing: Resource::PortRange {
                                base: conflicting_base,
                                size: conflicting_size,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }
                }
                Resource::SysReg { addr, count } => {
                    // Key collision.
                    if let Some(existing) = self.sysreg_index.get(&addr) {
                        let existing_count = existing.size as u32;
                        let existing_slot = existing.slot;
                        self.rollback_resources(&resources[..i]);
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::SysReg { addr, count },
                            existing: Resource::SysReg {
                                addr,
                                count: existing_count,
                            },
                            existing_device: DeviceId::new(existing_slot as u32),
                        });
                    }

                    let end = addr.saturating_add(count.saturating_sub(1));
                    self.sysreg_index.insert(
                        addr,
                        RangeEntry {
                            slot: idx,
                            size: count as u64,
                        },
                    );

                    // Predecessor check.
                    if let Some((prev_addr, existing)) = self.sysreg_index.range(..addr).next_back()
                        && prev_addr.saturating_add((existing.size as u32).saturating_sub(1))
                            >= addr
                    {
                        let conflicting_addr = *prev_addr;
                        let conflicting_count = existing.size as u32;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::SysReg { addr, count },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::SysReg { addr, count },
                            existing: Resource::SysReg {
                                addr: conflicting_addr,
                                count: conflicting_count,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }

                    // Successor check.
                    if let Some(next_addr) = addr.checked_add(1)
                        && let Some((reg_addr, existing)) =
                            self.sysreg_index.range(next_addr..).next()
                        && *reg_addr <= end
                    {
                        let conflicting_addr = *reg_addr;
                        let conflicting_count = existing.size as u32;
                        let conflicting_slot = existing.slot;
                        self.rollback_resources(&resources[..=i]);
                        if conflicting_slot == idx {
                            return Err(RegistryError::InvalidResource {
                                resource: Resource::SysReg { addr, count },
                                reason: InvalidResourceReason::OverlappingResources,
                            });
                        }
                        return Err(RegistryError::AddressConflict {
                            resource: Resource::SysReg { addr, count },
                            existing: Resource::SysReg {
                                addr: conflicting_addr,
                                count: conflicting_count,
                            },
                            existing_device: DeviceId::new(conflicting_slot as u32),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    // ─── Lookup helpers ────────────────────────────────────────────

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        let (&base, entry) = self.mmio_index.range(..=addr).next_back()?;
        (addr < base.wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        let (&base, entry) = self.port_index.range(..=addr).next_back()?;
        ((addr as u64) < (base as u64).wrapping_add(entry.size)).then_some(entry.slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        let (&start, entry) = self.sysreg_index.range(..=addr).next_back()?;
        let end = start.saturating_add((entry.size as u32).saturating_sub(1));
        (addr <= end).then_some(entry.slot)
    }

    // ─── Public helpers ───────────────────────────────────────────

    /// Returns an iterator over all currently registered devices.
    pub fn devices(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices.iter().map(|slot| &**slot)
    }

    /// Returns the number of currently registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    // ─── Iterator helpers ───────────────────────────────────────────
    //
    // NOTE: With the unified Device trait, [`devices()`] is the
    // canonical iterator.  Use [`Device::resources()`] or
    // [`Device::as_any()`] for per-bus filtering in new code.

    /// Iterates over devices that require periodic polling.
    pub fn iter_pollable_dev(&self) -> impl Iterator<Item = &Arc<dyn PollableDeviceOps>> {
        self.pollable_devices.iter()
    }

    // ─── x86 IOAPIC / PIT / Serial ──────────────────────────────────
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.x86_ioapic
            .as_ref()
            .and_then(|ioapic| ioapic.vector_for_gsi(gsi))
    }

    /// Signals an x86 IOAPIC GSI through its registered local-APIC output.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_signal_gsi(&self, gsi: usize) -> DeviceManagerResult<bool> {
        self.x86_ioapic
            .as_ref()
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "signal x86 IOAPIC GSI",
                resource: "x86 IOAPIC controller".into(),
            })?
            .signal_gsi(gsi)
    }

    /// Broadcast an x86 local APIC EOI to the virtual IOAPIC.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_ioapic_end_of_interrupt(
        &self,
        vector: u8,
    ) -> DeviceManagerResult<Option<IoApicEoi>> {
        self.x86_ioapic
            .as_ref()
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "complete x86 local APIC interrupt",
                resource: "x86 IOAPIC controller".into(),
            })?
            .end_of_interrupt(vector)
    }

    /// Consume a pending x86 PIT channel 0 timer tick if the deadline is due.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_pit_service_irq0(&self, now_ns: u64) -> DeviceManagerResult<bool> {
        self.x86_pit.as_ref().map_or(Ok(false), |pit| {
            pit.service_irq0(now_ns).map_err(Into::into)
        })
    }

    /// Poll x86 COM1 and return whether it has a pending RX interrupt.
    #[cfg(target_arch = "x86_64")]
    pub fn x86_serial_service_irq(&self) -> DeviceManagerResult<bool> {
        self.x86_serial
            .as_ref()
            .map_or(Ok(false), |serial| serial.service_irq().map_err(Into::into))
    }

    /// Atomically registers an x86 IOAPIC device and controller capabilities.
    #[cfg(target_arch = "x86_64")]
    pub fn add_x86_ioapic_controller<D, R>(
        &mut self,
        dev: Arc<D>,
        runtime: Arc<R>,
        controller: crate::ControllerRegistration,
        topology: &InterruptTopology,
    ) -> DeviceManagerResult
    where
        D: Device + 'static,
        R: X86IoApicRuntimeOps + 'static,
    {
        let bundle = DeviceBundle::new()
            .with_registration(DeviceRegistration::InterruptController(controller))
            .with_registration(DeviceRegistration::Device(dev as Arc<dyn Device>));
        self.register_bundle_with_topology(bundle, topology)?;
        self.x86_ioapic = Some(runtime);
        Ok(())
    }

    /// Atomically registers a LoongArch PCH-PIC device and interrupt controller.
    #[cfg(target_arch = "loongarch64")]
    pub fn add_loongarch_pch_pic_controller<R>(
        &mut self,
        dev: Arc<dyn Device>,
        runtime: Arc<R>,
        controller: crate::ControllerRegistration,
        topology: &InterruptTopology,
    ) -> DeviceManagerResult
    where
        R: LoongArchPchPicRuntimeOps + 'static,
    {
        let bundle = DeviceBundle::new()
            .with_registration(DeviceRegistration::InterruptController(controller))
            .with_registration(DeviceRegistration::Device(dev));
        self.register_bundle_with_topology(bundle, topology)?;
        self.loongarch_pch_pic = Some(runtime);
        Ok(())
    }

    /// Add an x86 PIT device to the generic registry and x86 runtime handle.
    #[cfg(target_arch = "x86_64")]
    pub fn add_x86_pit_dev<D>(&mut self, dev: Arc<D>) -> DeviceManagerResult
    where
        D: Device + X86PitDeviceOps + 'static,
    {
        self.register(dev.clone() as Arc<dyn Device>)?;
        self.x86_pit = Some(dev);
        Ok(())
    }

    /// Add an x86 COM1 device to the generic registry and x86 runtime handle.
    #[cfg(target_arch = "x86_64")]
    pub fn add_x86_serial_dev<D>(&mut self, dev: Arc<D>) -> DeviceManagerResult
    where
        D: Device + X86SerialDeviceOps + 'static,
    {
        self.register(dev.clone() as Arc<dyn Device>)?;
        self.x86_serial = Some(dev);
        Ok(())
    }

    /// Add a QEMU fw_cfg MMIO device to the device list.
    pub fn add_fw_cfg_dev(&mut self, dev: Arc<FwCfg>) -> DeviceManagerResult {
        self.register(
            MmioDeviceAdapter::from_arc(dev.clone()) as Arc<dyn Device + Send + Sync + 'static>
        )?;
        self.fw_cfg = Some(dev);
        Ok(())
    }

    /// Returns the fw_cfg device that owns `addr`, if any.
    pub fn fw_cfg_for_dma_addr(&self, addr: GuestPhysAddr) -> Option<Arc<FwCfg>> {
        self.fw_cfg
            .as_ref()
            .filter(|fw_cfg| fw_cfg.is_dma_address(addr))
            .cloned()
    }

    /// Routes LoongArch PCH-PIC output events generated by MMIO writes.
    #[cfg(target_arch = "loongarch64")]
    pub fn service_loongarch_pch_pic_outputs(&self) -> DeviceManagerResult {
        self.loongarch_pch_pic
            .as_ref()
            .map_or(Ok(()), |controller| controller.service_output_events())
    }

    // ─── Find helpers ───────────────────────────────────────────────

    /// Find specific MMIO device by ipa.
    /// Returns a reference to the underlying adapter which can be downcast
    /// via `as_any()`.
    pub fn find_mmio_dev(&self, ipa: GuestPhysAddr) -> Option<Arc<dyn Device>> {
        let access = BusAccess {
            kind: BusKind::Mmio,
            is_read: true,
            addr: ipa.as_usize() as u64,
            width: AccessWidth::Dword,
            data: 0,
        };
        self.lookup(&access).ok()
    }

    /// Find specific system register device by address.
    pub fn find_sys_reg_dev(&self, sys_reg_addr: SysRegAddr) -> Option<Arc<dyn Device>> {
        let access = BusAccess {
            kind: BusKind::SysReg,
            is_read: true,
            addr: sys_reg_addr.0 as u64,
            width: AccessWidth::Qword,
            data: 0,
        };
        self.lookup(&access).ok()
    }

    /// Find specific port device by port number.
    pub fn find_port_dev(&self, port: Port) -> Option<Arc<dyn Device>> {
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: true,
            addr: port.0 as u64,
            width: AccessWidth::Byte,
            data: 0,
        };
        self.lookup(&access).ok()
    }

    // ─── Hot-path dispatch handlers ─────────────────────────────────

    /// Handle the MMIO read by GuestPhysAddr and data width.
    pub fn handle_mmio_read(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
    ) -> DeviceManagerResult<usize> {
        let access = BusAccess {
            kind: BusKind::Mmio,
            is_read: true,
            addr: addr.as_usize() as u64,
            width,
            data: 0,
        };
        match self
            .dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "read",
                bus: BusKind::Mmio,
                addr: access.addr,
                width,
                source,
            })? {
            BusResponse::Read { value } => Ok(value as usize),
            BusResponse::Write => Err(DeviceManagerError::UnexpectedResponse {
                operation: "read MMIO device",
                detail: "device returned a write acknowledgement".into(),
            }),
        }
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write.
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceManagerResult {
        let access = BusAccess {
            kind: BusKind::Mmio,
            is_read: false,
            addr: addr.as_usize() as u64,
            width,
            data: val as u64,
        };
        self.dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "write",
                bus: BusKind::Mmio,
                addr: access.addr,
                width,
                source,
            })?;
        Ok(())
    }

    /// Handle the system register read by SysRegAddr and data width.
    pub fn handle_sys_reg_read(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
    ) -> DeviceManagerResult<usize> {
        let access = BusAccess {
            kind: BusKind::SysReg,
            is_read: true,
            addr: addr.0 as u64,
            width,
            data: 0,
        };
        match self
            .dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "read",
                bus: BusKind::SysReg,
                addr: access.addr,
                width,
                source,
            })? {
            BusResponse::Read { value } => Ok(value as usize),
            BusResponse::Write => Err(DeviceManagerError::UnexpectedResponse {
                operation: "read system register device",
                detail: "device returned a write acknowledgement".into(),
            }),
        }
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write.
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceManagerResult {
        let access = BusAccess {
            kind: BusKind::SysReg,
            is_read: false,
            addr: addr.0 as u64,
            width,
            data: val as u64,
        };
        self.dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "write",
                bus: BusKind::SysReg,
                addr: access.addr,
                width,
                source,
            })?;
        Ok(())
    }

    /// Handle the port read by port number and data width.
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> DeviceManagerResult<usize> {
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: true,
            addr: port.0 as u64,
            width,
            data: 0,
        };
        match self
            .dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "read",
                bus: BusKind::Port,
                addr: access.addr,
                width,
                source,
            })? {
            BusResponse::Read { value } => Ok(value as usize),
            BusResponse::Write => Err(DeviceManagerError::UnexpectedResponse {
                operation: "read port device",
                detail: "device returned a write acknowledgement".into(),
            }),
        }
    }

    /// Handle the port write by port number, data width and the value need to write.
    pub fn handle_port_write(
        &self,
        port: Port,
        width: AccessWidth,
        val: usize,
    ) -> DeviceManagerResult {
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: false,
            addr: port.0 as u64,
            width,
            data: val as u64,
        };
        self.dispatch(&access)
            .map_err(|source| DeviceManagerError::Access {
                operation: "write",
                bus: BusKind::Port,
                addr: access.addr,
                width,
                source,
            })?;
        Ok(())
    }
}

fn validate_resources(resources: &[Resource]) -> Result<(), RegistryError> {
    for resource in resources {
        let invalid_reason = match *resource {
            Resource::MmioRange { base, size } => (size == 0)
                .then_some(InvalidResourceReason::ZeroSized)
                .or_else(|| {
                    base.checked_add(size)
                        .is_none()
                        .then_some(InvalidResourceReason::AddressOverflow)
                }),
            Resource::PortRange { base, size } => (size == 0)
                .then_some(InvalidResourceReason::ZeroSized)
                .or_else(|| {
                    ((base as u32 + size as u32) > u16::MAX as u32 + 1)
                        .then_some(InvalidResourceReason::AddressOverflow)
                }),
            Resource::SysReg { addr, count } => (count == 0)
                .then_some(InvalidResourceReason::ZeroSized)
                .or_else(|| {
                    addr.checked_add(count.saturating_sub(1))
                        .is_none()
                        .then_some(InvalidResourceReason::AddressOverflow)
                }),
        };
        if let Some(reason) = invalid_reason {
            return Err(RegistryError::InvalidResource {
                resource: resource.clone(),
                reason,
            });
        }
    }
    Ok(())
}

impl Default for AxVmDevices {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl DeviceRegistry for AxVmDevices {
    fn register(&mut self, device: Arc<dyn Device>) -> Result<DeviceId, RegistryError> {
        let idx = self.devices.len();
        self.insert_resources(idx, device.resources())?;
        self.devices.push(device);
        info!("AxVmDevices: registered device id={}", idx);
        Ok(DeviceId::new(idx as u32))
    }
}

impl BusRouter for AxVmDevices {
    fn dispatch(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let idx = match access.kind {
            BusKind::Mmio => self.lookup_mmio(access.addr),
            BusKind::Port => {
                let port = u16::try_from(access.addr)
                    .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
                self.lookup_port(port)
            }
            BusKind::SysReg => {
                let reg = u32::try_from(access.addr)
                    .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
                self.lookup_sysreg(reg)
            }
        }
        .ok_or(DeviceError::NotFound)?;

        let device = &self.devices[idx];
        device.handle(access)
    }

    fn lookup(&self, access: &BusAccess) -> Result<Arc<dyn Device>, DeviceError> {
        let idx = match access.kind {
            BusKind::Mmio => self.lookup_mmio(access.addr),
            BusKind::Port => {
                let port = u16::try_from(access.addr)
                    .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
                self.lookup_port(port)
            }
            BusKind::SysReg => {
                let reg = u32::try_from(access.addr)
                    .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
                self.lookup_sysreg(reg)
            }
        }
        .ok_or(DeviceError::NotFound)?;

        Ok(Arc::clone(&self.devices[idx]))
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::any::Any;

    use axdevice_base::{
        AccessWidth, BusAccess, BusKind, BusResponse, BusRouter, Device, DeviceError,
        DeviceRegistry, InvalidResourceReason, Port, RegistryError, Resource, SysRegAddr,
    };
    use axvm_types::GuestPhysAddr;

    use super::AxVmDevices;

    struct D {
        resources: alloc::vec::Vec<Resource>,
        n: &'static str,
    }
    impl D {
        fn new_mmio(a: u64, s: u64, n: &'static str) -> Self {
            Self {
                resources: alloc::vec![Resource::MmioRange { base: a, size: s }],
                n,
            }
        }
        fn new_port(base: u16, size: u16, n: &'static str) -> Self {
            Self {
                resources: alloc::vec![Resource::PortRange { base, size }],
                n,
            }
        }
        fn new_sysreg(addr: u32, n: &'static str) -> Self {
            Self {
                resources: alloc::vec![Resource::SysReg { addr, count: 1 }],
                n,
            }
        }
    }
    impl Device for D {
        fn name(&self) -> &str {
            self.n
        }
        fn resources(&self) -> &[Resource] {
            &self.resources
        }
        fn handle(&self, _a: &BusAccess) -> Result<BusResponse, DeviceError> {
            Ok(BusResponse::Read { value: 0 })
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn test_register_dispatch() {
        let mut m = AxVmDevices::empty();
        m.register(Arc::new(D::new_mmio(0x1000, 0x100, "d")))
            .unwrap();
        assert!(
            m.dispatch(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0x1050,
                width: AccessWidth::Dword,
                data: 0
            })
            .is_ok()
        );
    }

    #[test]
    fn test_overlap() {
        let mut m = AxVmDevices::empty();
        m.register(Arc::new(D::new_mmio(0x1000, 0x200, "a")))
            .unwrap();
        assert!(matches!(
            m.register(Arc::new(D::new_mmio(0x1100, 0x100, "b"))),
            Err(RegistryError::AddressConflict { .. })
        ));
    }

    #[test]
    fn test_not_found() {
        assert!(matches!(
            AxVmDevices::empty().dispatch(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0xdead,
                width: AccessWidth::Dword,
                data: 0
            }),
            Err(DeviceError::NotFound)
        ));
    }

    #[test]
    fn test_port_sysreg() {
        let mut m = AxVmDevices::empty();
        m.register(Arc::new(D::new_port(0x80, 4, "p"))).unwrap();
        m.register(Arc::new(D::new_sysreg(0xC000, "s"))).unwrap();
        assert!(
            m.dispatch(&BusAccess {
                kind: BusKind::Port,
                is_read: true,
                addr: 0x80,
                width: AccessWidth::Byte,
                data: 0
            })
            .is_ok()
        );
        assert!(
            m.dispatch(&BusAccess {
                kind: BusKind::SysReg,
                is_read: true,
                addr: 0xC000,
                width: AccessWidth::Qword,
                data: 0
            })
            .is_ok()
        );
    }

    #[test]
    fn test_same_device_overlapping_mmio_rejected() {
        // Same device declaring [0x1000, 0x1200) and [0x1100, 0x1300)
        struct OverlapDevice;
        impl Device for OverlapDevice {
            fn name(&self) -> &str {
                "overlap"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 2] = [
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x200,
                    },
                    Resource::MmioRange {
                        base: 0x1100,
                        size: 0x200,
                    },
                ];
                &R
            }
            fn handle(&self, _: &BusAccess) -> Result<BusResponse, DeviceError> {
                Ok(BusResponse::Read { value: 0 })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        let result = m.register(Arc::new(OverlapDevice));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::OverlappingResources,
                ..
            })
        ));
    }

    #[test]
    fn test_same_device_nested_mmio_rejected() {
        // Same device declaring [0x1000, 0x2000) and [0x1800, 0x1900) —
        // smaller range is fully inside larger range.
        struct NestedDevice;
        impl Device for NestedDevice {
            fn name(&self) -> &str {
                "nested"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 2] = [
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x1000,
                    },
                    Resource::MmioRange {
                        base: 0x1800,
                        size: 0x100,
                    },
                ];
                &R
            }
            fn handle(&self, _: &BusAccess) -> Result<BusResponse, DeviceError> {
                Ok(BusResponse::Read { value: 0 })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        let result = m.register(Arc::new(NestedDevice));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::OverlappingResources,
                ..
            })
        ));
    }

    #[test]
    fn test_same_device_mmio_port_same_addr_allowed() {
        // Same numeric address on different buses is allowed.
        struct DualBusDevice;
        impl Device for DualBusDevice {
            fn name(&self) -> &str {
                "dual-bus"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 2] = [
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x100,
                    },
                    Resource::PortRange {
                        base: 0x1000,
                        size: 0x10,
                    },
                ];
                &R
            }
            fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
                if access.is_read {
                    Ok(BusResponse::Read { value: 0 })
                } else {
                    Ok(BusResponse::Write)
                }
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        assert!(m.register(Arc::new(DualBusDevice)).is_ok());
    }

    #[test]
    fn test_sysreg_max_single_register_valid() {
        // addr = u32::MAX, count = 1 is the highest valid single-register
        // range and should not be rejected as overflow.
        struct MaxSysRegDevice;
        impl Device for MaxSysRegDevice {
            fn name(&self) -> &str {
                "max-sysreg"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 1] = [Resource::SysReg {
                    addr: u32::MAX,
                    count: 1,
                }];
                &R
            }
            fn handle(&self, _: &BusAccess) -> Result<BusResponse, DeviceError> {
                Ok(BusResponse::Read { value: 0 })
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        assert!(m.register(Arc::new(MaxSysRegDevice)).is_ok());
    }

    #[test]
    fn test_read_request_rejects_write_response() {
        // A device that incorrectly returns BusResponse::Write for a read
        // should cause the handle_*_read methods to return an error.
        // The device declares a resource on each bus so that the lookup
        // actually finds it instead of returning NotFound.
        struct WriteOnlyDevice;
        impl Device for WriteOnlyDevice {
            fn name(&self) -> &str {
                "write-only"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 3] = [
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x100,
                    },
                    Resource::PortRange {
                        base: 0x1000,
                        size: 0x10,
                    },
                    Resource::SysReg {
                        addr: 0x1000,
                        count: 1,
                    },
                ];
                &R
            }
            fn handle(&self, _access: &BusAccess) -> Result<BusResponse, DeviceError> {
                Ok(BusResponse::Write)
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        m.register(Arc::new(WriteOnlyDevice)).unwrap();

        // handle_mmio_read should detect the mismatched response.
        let result = m.handle_mmio_read(GuestPhysAddr::from(0x1000), AccessWidth::Dword);
        assert!(result.is_err());

        // handle_sys_reg_read should also detect it.
        let result = m.handle_sys_reg_read(SysRegAddr::new(0x1000), AccessWidth::Qword);
        assert!(result.is_err());

        // handle_port_read should also detect it.
        let result = m.handle_port_read(Port::new(0x1000), AccessWidth::Byte);
        assert!(result.is_err());
    }

    #[test]
    fn test_write_request_returns_write_response() {
        struct RwDevice;
        impl Device for RwDevice {
            fn name(&self) -> &str {
                "rw"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 1] = [Resource::MmioRange {
                    base: 0x1000,
                    size: 0x100,
                }];
                &R
            }
            fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
                if access.is_read {
                    Ok(BusResponse::Read { value: 0 })
                } else {
                    Ok(BusResponse::Write)
                }
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        m.register(Arc::new(RwDevice)).unwrap();
        let resp = m
            .dispatch(&BusAccess {
                kind: BusKind::Mmio,
                is_read: false,
                addr: 0x1000,
                width: AccessWidth::Dword,
                data: 0x42,
            })
            .unwrap();
        assert!(matches!(resp, BusResponse::Write));
    }

    #[test]
    fn test_port_max_address_valid() {
        let mut m = AxVmDevices::empty();
        m.register(Arc::new(D::new_port(0xffff, 1, "max-port")))
            .unwrap();
        assert!(
            m.dispatch(&BusAccess {
                kind: BusKind::Port,
                is_read: true,
                addr: 0xffff,
                width: AccessWidth::Byte,
                data: 0
            })
            .is_ok()
        );
    }

    #[test]
    fn test_zero_size_returns_invalid_resource() {
        let mut m = AxVmDevices::empty();
        let result = m.register(Arc::new(D::new_mmio(0x1000, 0, "zero")));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::ZeroSized,
                ..
            })
        ));
    }

    #[test]
    fn invalid_late_resource_rolls_back_all_earlier_indices() {
        for invalid in [
            Resource::MmioRange {
                base: 0x2000,
                size: 0,
            },
            Resource::MmioRange {
                base: u64::MAX - 1,
                size: 4,
            },
        ] {
            let mut devices = AxVmDevices::empty();
            let result = devices.register(Arc::new(D {
                resources: alloc::vec![
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x100,
                    },
                    invalid,
                ],
                n: "invalid-late-resource",
            }));
            assert!(matches!(result, Err(RegistryError::InvalidResource { .. })));

            devices
                .register(Arc::new(D::new_mmio(0x1000, 0x100, "replacement")))
                .expect("the valid prefix of a rejected device must be rolled back");
        }
    }

    #[test]
    fn test_mmio_overflow_returns_invalid_resource() {
        struct OverflowDevice;
        impl Device for OverflowDevice {
            fn name(&self) -> &str {
                "overflow"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 1] = [Resource::MmioRange {
                    base: u64::MAX - 1,
                    size: 4,
                }];
                &R
            }
            fn handle(&self, _: &BusAccess) -> Result<BusResponse, DeviceError> {
                Err(DeviceError::NotFound)
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let mut m = AxVmDevices::empty();
        let result = m.register(Arc::new(OverflowDevice));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::AddressOverflow,
                ..
            })
        ));
    }

    #[test]
    fn test_access_across_resource_boundary() {
        // Access that starts inside a device's range but with a larger
        // width still dispatches to the matching device.
        let mut m = AxVmDevices::empty();
        m.register(Arc::new(D::new_mmio(0x1000, 0x8, "small")))
            .unwrap();
        assert!(
            m.dispatch(&BusAccess {
                kind: BusKind::Mmio,
                is_read: false,
                addr: 0x1004,
                width: AccessWidth::Qword,
                data: 0,
            })
            .is_ok()
        );
        // 0x1008 == base + size — NotFound.
        assert!(matches!(
            m.dispatch(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0x1008,
                width: AccessWidth::Dword,
                data: 0
            }),
            Err(DeviceError::NotFound)
        ));
    }
}
