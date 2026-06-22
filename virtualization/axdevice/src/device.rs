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

#[cfg(target_arch = "aarch64")]
use arm_vgic::Vgic;
use ax_errno::{AxResult, ax_err, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(target_arch = "aarch64")]
use ax_memory_addr::PhysAddr;
use ax_memory_addr::is_aligned_4k;
#[cfg(target_arch = "x86_64")]
use axdevice_base::PortDeviceAdapter;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, BusRouter, Device, DeviceError, DeviceId,
    DeviceRegistry, MmioDeviceAdapter, Port, RegistryError, Resource, SysRegAddr,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr};
#[cfg(target_arch = "riscv64")]
use riscv_vplic::VPlicGlobal;
#[cfg(target_arch = "x86_64")]
use x86_vlapic::{EmulatedIoApic, EmulatedPit, EmulatedSerialPort, IoApicInterrupt};

use crate::{
    AxVmDeviceConfig, DeviceBuildContext, DeviceBundle, DeviceFactoryRegistry, PollableDeviceOps,
    range_alloc::RangeAllocator,
};

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

/// represent A vm own devices
pub struct AxVmDevices {
    /// Device slots (None = freed and available for reuse).
    devices: Vec<Option<Arc<dyn Device>>>,
    /// MMIO base address → device slot index.
    mmio_index: BTreeMap<u64, usize>,
    /// Port I/O base address → device slot index.
    port_index: BTreeMap<u16, usize>,
    /// System register address → device slot index.
    sysreg_index: BTreeMap<u32, usize>,
    /// Devices that require periodic polling.
    pollable_devices: Vec<Arc<dyn PollableDeviceOps>>,
    /// x86 IOAPIC — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_ioapic: Option<Arc<EmulatedIoApic>>,
    /// x86 PIT — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_pit: Option<Arc<EmulatedPit>>,
    /// x86 16550 serial port — kept for type-specific access.
    #[cfg(target_arch = "x86_64")]
    x86_serial: Option<Arc<EmulatedSerialPort>>,
    /// IVC channel range allocator
    ivc_channel: Option<Mutex<RangeAllocator>>,
}

/// The implemention for AxVmDevices
impl AxVmDevices {
    fn empty() -> Self {
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
            ivc_channel: None,
        }
    }

    /// According AxVmDeviceConfig to init the AxVmDevices
    pub fn new(config: AxVmDeviceConfig) -> AxResult<Self> {
        let mut this = Self::empty();

        Self::init(&mut this, &config.emu_configs)?;
        Ok(this)
    }

    /// Builds devices with registered factories and explicit legacy fallbacks.
    pub fn build_with_factories(
        config: AxVmDeviceConfig,
        factories: &DeviceFactoryRegistry,
        context: &DeviceBuildContext<'_>,
    ) -> AxResult<Self> {
        let mut this = Self::empty();
        for config in &config.emu_configs {
            if factories.get(config.emu_type).is_some() {
                this.register_factory_device(config, factories, context)?;
            } else if Self::is_legacy_fallback(config.emu_type) {
                Self::init(&mut this, core::slice::from_ref(config))?;
            } else {
                return ax_err!(
                    Unsupported,
                    format_args!(
                        "no factory is registered for emulated device '{}' of type {}",
                        config.name, config.emu_type
                    )
                );
            }
        }
        Ok(this)
    }

    /// Builds and atomically registers one factory-managed device.
    pub fn register_factory_device(
        &mut self,
        config: &EmulatedDeviceConfig,
        factories: &DeviceFactoryRegistry,
        context: &DeviceBuildContext<'_>,
    ) -> AxResult {
        let bundle = factories.build(config, context)?;
        self.register_bundle(bundle)
    }

    fn is_legacy_fallback(device_type: EmulatedDeviceType) -> bool {
        matches!(
            device_type,
            EmulatedDeviceType::InterruptController
                | EmulatedDeviceType::Console
                | EmulatedDeviceType::IVCChannel
                | EmulatedDeviceType::GPPTRedistributor
                | EmulatedDeviceType::GPPTDistributor
                | EmulatedDeviceType::GPPTITS
                | EmulatedDeviceType::X86IoApic
                | EmulatedDeviceType::X86Pit
                | EmulatedDeviceType::PPPTGlobal
        )
    }

    /// According the emu_configs to init every  specific device
    fn init(this: &mut Self, emu_configs: &[EmulatedDeviceConfig]) -> AxResult {
        for config in emu_configs {
            match config.emu_type {
                EmulatedDeviceType::InterruptController => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        #[allow(clippy::arc_with_non_send_sync)]
                        this.register(
                            MmioDeviceAdapter::from_arc(Arc::new(Vgic::new())) as Arc<dyn Device>
                        )
                        .map_err(|e| {
                            ax_err_type!(InvalidInput, alloc::format!("register vgic: {e:?}"))
                        })?;
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
                    #[cfg(target_arch = "aarch64")]
                    {
                        const GPPT_GICR_ARG_ERR_MSG: &str =
                            "expect 3 args for gppt redistributor (cpu_num, stride, pcpu_id)";

                        let cpu_num = config
                            .cfg_list
                            .first()
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);
                        let stride = config
                            .cfg_list
                            .get(1)
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);
                        let pcpu_id = config
                            .cfg_list
                            .get(2)
                            .copied()
                            .expect(GPPT_GICR_ARG_ERR_MSG);

                        for i in 0..cpu_num {
                            let addr = config.base_gpa + i * stride;
                            let size = config.length;
                            #[allow(clippy::arc_with_non_send_sync)]
                            #[allow(clippy::arc_with_non_send_sync)]
                            this.register(MmioDeviceAdapter::from_arc(Arc::new(
                                arm_vgic::v3::vgicr::VGicR::new(
                                    addr.into(),
                                    Some(size),
                                    pcpu_id + i,
                                ),
                            )) as Arc<dyn Device>)
                                .map_err(|e| {
                                    ax_err_type!(
                                        InvalidInput,
                                        alloc::format!("register gicr: {e:?}")
                                    )
                                })?;

                            info!(
                                "GPPT Redistributor initialized for vCPU {i} with base GPA \
                                 {addr:#x} and length {size:#x}"
                            );
                        }
                    }
                    #[cfg(not(target_arch = "aarch64"))]
                    {
                        warn!(
                            "emu type: {} is not supported on this platform",
                            config.emu_type
                        );
                    }
                }
                EmulatedDeviceType::GPPTDistributor => {
                    #[cfg(target_arch = "aarch64")]
                    {
                        #[allow(clippy::arc_with_non_send_sync)]
                        this.register(MmioDeviceAdapter::from_arc(Arc::new(
                            arm_vgic::v3::vgicd::VGicD::new(
                                config.base_gpa.into(),
                                Some(config.length),
                            ),
                        )) as Arc<dyn Device>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, alloc::format!("register gicd: {e:?}"))
                            })?;

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
                        this.register(MmioDeviceAdapter::from_arc(Arc::new(
                            arm_vgic::v3::gits::Gits::new(
                                config.base_gpa.into(),
                                Some(config.length),
                                host_gits_base,
                                false,
                            ),
                        )) as Arc<dyn Device>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, alloc::format!("register gits: {e:?}"))
                            })?;

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
                        this.register(MmioDeviceAdapter::from_arc(Arc::new(VPlicGlobal::new(
                            config.base_gpa.into(),
                            Some(config.length),
                            context_num,
                        ))) as Arc<dyn Device>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, alloc::format!("register pppt: {e:?}"))
                            })?;
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
                        this.register(PortDeviceAdapter::from_arc(serial.clone())
                            as Arc<dyn Device + Send + Sync + 'static>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, format!("register x86 serial: {e:?}"))
                            })?;
                        this.x86_serial = Some(serial);
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
                        this.register(MmioDeviceAdapter::from_arc(ioapic.clone())
                            as Arc<dyn Device + Send + Sync + 'static>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, format!("register x86 ioapic: {e:?}"))
                            })?;
                        this.x86_ioapic = Some(ioapic);
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
                        this.register(PortDeviceAdapter::from_arc(pit.clone()) as Arc<dyn Device>)
                            .map_err(|e| {
                                ax_err_type!(InvalidInput, format!("register x86 pit: {e:?}"))
                            })?;
                        this.x86_pit = Some(pit);
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
        Ok(())
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

    /// Registers a bundle atomically.  Conflict detection is performed by
    /// [`DeviceRegistry::register`].  If any device fails to register,
    /// already-registered devices in this bundle are rolled back.
    pub fn register_bundle(&mut self, bundle: DeviceBundle) -> AxResult {
        for (index, pollable) in bundle.pollable.iter().enumerate() {
            if self
                .pollable_devices
                .iter()
                .chain(bundle.pollable[..index].iter())
                .any(|existing| Arc::ptr_eq(existing, pollable))
            {
                return ax_err!(
                    AlreadyExists,
                    "failed to register pollable device: the same capability is already registered"
                );
            }
        }

        let mut registered_ids: Vec<DeviceId> = Vec::new();
        for device in &bundle.devices {
            match self.register(device.clone()) {
                Ok(id) => registered_ids.push(id),
                Err(e) => {
                    // Rollback already-registered devices.
                    for id in registered_ids.iter().rev() {
                        let _ = self.unregister(*id);
                    }
                    return Err(ax_err_type!(
                        InvalidInput,
                        format!("device registration failed: {e:?}")
                    ));
                }
            }
        }
        self.pollable_devices.extend(bundle.pollable);
        Ok(())
    }

    // ─── Registration helpers ───────────────────────────────────────

    /// Finds a free slot in `devices` or appends a new one, returning the
    /// index.
    fn alloc_slot(&mut self) -> usize {
        match self.devices.iter().position(|slot| slot.is_none()) {
            Some(idx) => idx,
            None => {
                self.devices.push(None);
                self.devices.len() - 1
            }
        }
    }

    /// Checks whether `[base, base + size)` conflicts with any already
    /// registered MMIO range.
    fn check_mmio_conflict(&self, base: u64, size: u64) -> Result<(), RegistryError> {
        if size == 0 {
            return Err(RegistryError::AddressConflict {
                resource: Resource::MmioRange { base, size },
                existing: Resource::MmioRange { base: 0, size: 0 },
                existing_device: DeviceId::new(0),
            });
        }

        // Check the immediately-preceding entry.
        if let Some((prev_base, &prev_idx)) = self.mmio_index.range(..base).next_back()
            && let Some((existing_base, existing_size)) =
                self.mmio_range_of_device(prev_idx, *prev_base)
            && existing_base + existing_size > base
        {
            return Err(RegistryError::AddressConflict {
                resource: Resource::MmioRange { base, size },
                existing: Resource::MmioRange {
                    base: existing_base,
                    size: existing_size,
                },
                existing_device: DeviceId::new(prev_idx as u32),
            });
        }

        // Check the immediately-following entry.
        let end = base + size;
        if let Some((next_base, &next_idx)) = self.mmio_index.range(base..).next()
            && *next_base < end
            && let Some((existing_base, existing_size)) =
                self.mmio_range_of_device(next_idx, *next_base)
        {
            return Err(RegistryError::AddressConflict {
                resource: Resource::MmioRange { base, size },
                existing: Resource::MmioRange {
                    base: existing_base,
                    size: existing_size,
                },
                existing_device: DeviceId::new(next_idx as u32),
            });
        }

        Ok(())
    }

    /// Checks whether `[base, base + size)` conflicts with any already
    /// registered port I/O range.
    fn check_port_conflict(&self, base: u16, size: u16) -> Result<(), RegistryError> {
        if size == 0 {
            return Err(RegistryError::AddressConflict {
                resource: Resource::PortRange { base, size },
                existing: Resource::PortRange { base: 0, size: 0 },
                existing_device: DeviceId::new(0),
            });
        }

        if let Some((prev_base, &prev_idx)) = self.port_index.range(..base).next_back()
            && let Some((existing_base, existing_size)) =
                self.port_range_of_device(prev_idx, *prev_base)
            && existing_base + existing_size > base
        {
            return Err(RegistryError::AddressConflict {
                resource: Resource::PortRange { base, size },
                existing: Resource::PortRange {
                    base: existing_base,
                    size: existing_size,
                },
                existing_device: DeviceId::new(prev_idx as u32),
            });
        }

        let end = base + size;
        if let Some((next_base, &next_idx)) = self.port_index.range(base..).next()
            && *next_base < end
            && let Some((existing_base, existing_size)) =
                self.port_range_of_device(next_idx, *next_base)
        {
            return Err(RegistryError::AddressConflict {
                resource: Resource::PortRange { base, size },
                existing: Resource::PortRange {
                    base: existing_base,
                    size: existing_size,
                },
                existing_device: DeviceId::new(next_idx as u32),
            });
        }

        Ok(())
    }

    /// Checks whether a system register range conflicts with any already
    /// registered system register range.
    fn check_sysreg_conflict(&self, addr: u32, count: u32) -> Result<(), RegistryError> {
        if count == 0 {
            return Err(RegistryError::AddressConflict {
                resource: Resource::SysReg { addr, count },
                existing: Resource::SysReg { addr: 0, count: 0 },
                existing_device: DeviceId::new(0),
            });
        }

        let end = addr.saturating_add(count.saturating_sub(1));

        // Check the immediately-preceding key: its range may extend into ours.
        if let Some((prev_addr, &prev_idx)) = self.sysreg_index.range(..addr).next_back() {
            let existing_count = self
                .sysreg_count_of_device(prev_idx, *prev_addr)
                .unwrap_or(1);
            let existing_end = prev_addr.saturating_add(existing_count.saturating_sub(1));
            if existing_end >= addr {
                return Err(RegistryError::AddressConflict {
                    resource: Resource::SysReg { addr, count },
                    existing: Resource::SysReg {
                        addr: *prev_addr,
                        count: existing_count,
                    },
                    existing_device: DeviceId::new(prev_idx as u32),
                });
            }
        }

        // Check entries whose keys fall within our range.
        if let Some((reg_addr, &idx)) = self.sysreg_index.range(addr..=end).next() {
            let existing_count = self.sysreg_count_of_device(idx, *reg_addr).unwrap_or(1);
            return Err(RegistryError::AddressConflict {
                resource: Resource::SysReg { addr, count },
                existing: Resource::SysReg {
                    addr: *reg_addr,
                    count: existing_count,
                },
                existing_device: DeviceId::new(idx as u32),
            });
        }

        Ok(())
    }

    fn sysreg_count_of_device(&self, idx: usize, addr: u32) -> Option<u32> {
        self.devices[idx].as_ref().and_then(|d| {
            d.resources().into_iter().find_map(|r| match r {
                Resource::SysReg { addr: a, count: c } if a == addr => Some(c),
                _ => None,
            })
        })
    }

    /// Returns the `(base, size)` of the `MmioRange` resource of
    /// `devices[idx]` whose `base` matches.
    fn mmio_range_of_device(&self, idx: usize, base: u64) -> Option<(u64, u64)> {
        let dev = self.devices[idx].as_ref()?;
        dev.resources().into_iter().find_map(|r| match r {
            Resource::MmioRange { base: b, size: s } if b == base => Some((b, s)),
            _ => None,
        })
    }

    /// Returns the `(base, size)` of the `PortRange` resource of
    /// `devices[idx]` whose `base` matches.
    fn port_range_of_device(&self, idx: usize, base: u16) -> Option<(u16, u16)> {
        let dev = self.devices[idx].as_ref()?;
        dev.resources().into_iter().find_map(|r| match r {
            Resource::PortRange { base: b, size: s } if b == base => Some((b, s)),
            _ => None,
        })
    }

    // ─── Lookup helpers ────────────────────────────────────────────

    fn lookup_mmio(&self, addr: u64) -> Option<usize> {
        let (&base, &idx) = self.mmio_index.range(..=addr).next_back()?;
        let dev = self.devices[idx].as_ref()?;
        let in_range = dev.resources().into_iter().any(|r| {
            matches!(r,
                Resource::MmioRange { base: b, size: s }
                if b == base && addr < b + s)
        });
        in_range.then_some(idx)
    }

    fn lookup_port(&self, addr: u16) -> Option<usize> {
        let (&base, &idx) = self.port_index.range(..=addr).next_back()?;
        let dev = self.devices[idx].as_ref()?;
        let in_range = dev.resources().into_iter().any(|r| {
            matches!(r,
                Resource::PortRange { base: b, size: s }
                if b == base && addr < b + s)
        });
        in_range.then_some(idx)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        self.sysreg_index.get(&addr).copied()
    }

    // ─── BTreeMap insertion / removal ──────────────────────────────

    fn insert_resources(&mut self, idx: usize, resources: &[Resource]) {
        for r in resources {
            match *r {
                Resource::MmioRange { base, .. } => {
                    self.mmio_index.insert(base, idx);
                }
                Resource::PortRange { base, .. } => {
                    self.port_index.insert(base, idx);
                }
                Resource::SysReg { addr, .. } => {
                    self.sysreg_index.insert(addr, idx);
                }
                _ => {}
            }
        }
    }

    // ─── Public helpers ───────────────────────────────────────────

    /// Returns an iterator over all currently registered devices.
    pub fn devices(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices.iter().filter_map(|slot| slot.as_deref())
    }

    /// Returns the number of currently registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.iter().filter(|slot| slot.is_some()).count()
    }

    // ─── Iterator helpers ───────────────────────────────────────────

    /// Iterates over all registered devices.
    pub fn iter_mmio_dev(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices()
    }

    /// Iterates over all registered devices.
    pub fn iter_sys_reg_dev(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices()
    }

    /// Iterates over all registered devices.
    pub fn iter_port_dev(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices()
    }

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
    pub fn handle_mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> AxResult<usize> {
        let access = BusAccess {
            kind: BusKind::Mmio,
            is_read: true,
            addr: addr.as_usize() as u64,
            width,
            data: 0,
        };
        match self.dispatch(&access) {
            Ok(BusResponse::Read { value }) => Ok(value as usize),
            Ok(BusResponse::Write) => {
                Err(ax_err_type!(BadState, "expected read response, got write"))
            }
            Err(err) => {
                error!("emu_device mmio read failed: {err:?} at {addr:#x} width {width:?}");
                Err(ax_err_type!(BadState, format!("mmio read: {err:?}")))
            }
        }
    }

    /// Handle the MMIO write by GuestPhysAddr, data width and the value need to write.
    pub fn handle_mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        let access = BusAccess {
            kind: BusKind::Mmio,
            is_read: false,
            addr: addr.as_usize() as u64,
            width,
            data: val as u64,
        };
        if let Err(err) = self.dispatch(&access) {
            error!("emu_device mmio write failed: {err:?} at {addr:#x} width {width:?}");
            return Err(ax_err_type!(BadState, format!("mmio write: {err:?}")));
        }
        Ok(())
    }

    /// Handle the system register read by SysRegAddr and data width.
    pub fn handle_sys_reg_read(&self, addr: SysRegAddr, width: AccessWidth) -> AxResult<usize> {
        let access = BusAccess {
            kind: BusKind::SysReg,
            is_read: true,
            addr: addr.0 as u64,
            width,
            data: 0,
        };
        match self.dispatch(&access) {
            Ok(BusResponse::Read { value }) => Ok(value as usize),
            Ok(BusResponse::Write) => {
                Err(ax_err_type!(BadState, "expected read response, got write"))
            }
            Err(err) => {
                error!(
                    "emu_device sys_reg read failed: {err:?} at {:#x} width {width:?}",
                    addr.0
                );
                Err(ax_err_type!(BadState, format!("sysreg read: {err:?}")))
            }
        }
    }

    /// Handle the system register write by SysRegAddr, data width and the value need to write.
    pub fn handle_sys_reg_write(
        &self,
        addr: SysRegAddr,
        width: AccessWidth,
        val: usize,
    ) -> AxResult {
        let access = BusAccess {
            kind: BusKind::SysReg,
            is_read: false,
            addr: addr.0 as u64,
            width,
            data: val as u64,
        };
        if let Err(err) = self.dispatch(&access) {
            error!(
                "emu_device sys_reg write failed: {err:?} at {:#x} width {width:?}",
                addr.0
            );
            return Err(ax_err_type!(BadState, format!("sysreg write: {err:?}")));
        }
        Ok(())
    }

    /// Handle the port read by port number and data width.
    pub fn handle_port_read(&self, port: Port, width: AccessWidth) -> AxResult<usize> {
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: true,
            addr: port.0 as u64,
            width,
            data: 0,
        };
        match self.dispatch(&access) {
            Ok(BusResponse::Read { value }) => Ok(value as usize),
            Ok(BusResponse::Write) => {
                Err(ax_err_type!(BadState, "expected read response, got write"))
            }
            Err(err) => {
                error!(
                    "emu_device port read failed: {err:?} at {:#x} width {width:?}",
                    port.0
                );
                Err(ax_err_type!(BadState, format!("port read: {err:?}")))
            }
        }
    }

    /// Handle the port write by port number, data width and the value need to write.
    pub fn handle_port_write(&self, port: Port, width: AccessWidth, val: usize) -> AxResult {
        let access = BusAccess {
            kind: BusKind::Port,
            is_read: false,
            addr: port.0 as u64,
            width,
            data: val as u64,
        };
        if let Err(err) = self.dispatch(&access) {
            error!(
                "emu_device port write failed: {err:?} at {:#x} width {width:?}",
                port.0
            );
            return Err(ax_err_type!(BadState, format!("port write: {err:?}")));
        }
        Ok(())
    }
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
        let resources = device.resources();

        // 1. Conflict detection (only for address-range resources).
        for r in &resources {
            match *r {
                Resource::MmioRange { base, size } => self.check_mmio_conflict(base, size)?,
                Resource::PortRange { base, size } => self.check_port_conflict(base, size)?,
                Resource::SysReg { addr, count } => self.check_sysreg_conflict(addr, count)?,
                _ => { /* Irq / MSI / PciBar / DmaCapable — handled by factory */ }
            }
        }

        // 2. Allocate a slot.
        let idx = self.alloc_slot();

        // 3. Insert into index maps.
        self.insert_resources(idx, &resources);

        // 4. Store the device.
        self.devices[idx] = Some(device);

        info!("AxVmDevices: registered device id={}", idx);
        Ok(DeviceId::new(idx as u32))
    }

    fn unregister(&mut self, id: DeviceId) -> Result<(), RegistryError> {
        let idx = id.as_u32() as usize;

        let device = self
            .devices
            .get(idx)
            .and_then(|slot| slot.as_ref())
            .ok_or_else(|| {
                warn!(
                    "AxVmDevices: unregister failed — DeviceId({}) not found",
                    idx
                );
                RegistryError::DeviceNotFound(id)
            })?;

        // Remove from all index maps.
        let resources = device.resources();
        for r in &resources {
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
                _ => {}
            }
        }

        // Free the slot.
        self.devices[idx] = None;

        info!("AxVmDevices: unregistered device id={}", idx);
        Ok(())
    }
}

impl BusRouter for AxVmDevices {
    fn dispatch(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let idx = match access.kind {
            BusKind::Mmio => self.lookup_mmio(access.addr),
            BusKind::Port => self.lookup_port(access.addr as u16),
            BusKind::SysReg => self.lookup_sysreg(access.addr as u32),
        }
        .ok_or(DeviceError::NotFound)?;

        let device = self.devices[idx].as_ref().ok_or(DeviceError::NotFound)?;
        device.handle(access)
    }

    fn lookup(&self, access: &BusAccess) -> Result<Arc<dyn Device>, DeviceError> {
        let idx = match access.kind {
            BusKind::Mmio => self.lookup_mmio(access.addr),
            BusKind::Port => self.lookup_port(access.addr as u16),
            BusKind::SysReg => self.lookup_sysreg(access.addr as u32),
        }
        .ok_or(DeviceError::NotFound)?;

        self.devices[idx].clone().ok_or(DeviceError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::any::Any;

    use axdevice_base::{
        AccessWidth, BusAccess, BusKind, BusResponse, BusRouter, Device, DeviceError,
        DeviceRegistry, RegistryError, Resource,
    };

    use super::AxVmDevices;

    struct D {
        a: u64,
        s: u64,
        n: &'static str,
        k: BusKind,
    }
    impl Device for D {
        fn name(&self) -> &str {
            self.n
        }
        fn resources(&self) -> Vec<Resource> {
            let mut v = Vec::new();
            match self.k {
                BusKind::Mmio => v.push(Resource::MmioRange {
                    base: self.a,
                    size: self.s,
                }),
                BusKind::Port => v.push(Resource::PortRange {
                    base: self.a as u16,
                    size: self.s as u16,
                }),
                BusKind::SysReg => v.push(Resource::SysReg {
                    addr: self.a as u32,
                    count: 1,
                }),
            }
            v
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
        m.register(Arc::new(D {
            a: 0x1000,
            s: 0x100,
            n: "d",
            k: BusKind::Mmio,
        }))
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
        m.register(Arc::new(D {
            a: 0x1000,
            s: 0x200,
            n: "a",
            k: BusKind::Mmio,
        }))
        .unwrap();
        assert!(matches!(
            m.register(Arc::new(D {
                a: 0x1100,
                s: 0x100,
                n: "b",
                k: BusKind::Mmio
            })),
            Err(RegistryError::AddressConflict { .. })
        ));
    }

    #[test]
    fn test_reuse() {
        let mut m = AxVmDevices::empty();
        let id = m
            .register(Arc::new(D {
                a: 0x1000,
                s: 0x100,
                n: "a",
                k: BusKind::Mmio,
            }))
            .unwrap();
        m.unregister(id).unwrap();
        assert_eq!(
            id,
            m.register(Arc::new(D {
                a: 0x2000,
                s: 0x100,
                n: "b",
                k: BusKind::Mmio
            }))
            .unwrap()
        );
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
        m.register(Arc::new(D {
            a: 0x80,
            s: 4,
            n: "p",
            k: BusKind::Port,
        }))
        .unwrap();
        m.register(Arc::new(D {
            a: 0xC000,
            s: 4,
            n: "s",
            k: BusKind::SysReg,
        }))
        .unwrap();
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
}
