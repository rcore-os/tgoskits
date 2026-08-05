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

//! Runtime configuration structures for an AxVM instance.

use alloc::{string::String, sync::Arc, vec::Vec};

use axdevice::{NullSerialBackendFactory, SerialBackendFactory};
pub use axvm_types::{
    AddressSpacePolicy, EmulatedDeviceConfig, GuestPhysAddr, PassThroughAddressConfig,
    PassThroughDeviceConfig, PassThroughPortConfig, ReservedAddressConfig, VMBootProtocol,
    VMInterruptMode, VmMemConfig, VmMemMappingType,
};
use axvm_types::{EmulatedDeviceType, InterruptTriggerMode};

use crate::{
    arch::{ArchOps, CurrentArch},
    machine::{
        GuestGicCpuRegion, GuestGicProfile, GuestPlicProfile, GuestSerialFdtIdentity,
        GuestSerialProfile, GuestTimerProfile,
    },
};

/// Policy used by AxVM when deriving runtime guest boot image addresses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuestBootPolicy {
    /// Keep the load addresses exactly as provided by the VM config.
    #[default]
    KeepConfigured,
    /// Adjust the kernel load address for boot protocols that require a
    /// reserved area inside the primary guest memory region.
    AdjustKernelForBootProtocol { protocol: VMBootProtocol },
}

/// A part of `AxVMConfig`, which represents a `VCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVCpuConfig {
    /// The entry address in GPA for the Bootstrap Processor (BSP).
    pub bsp_entry: GuestPhysAddr,
    /// The entry address in GPA for the Application Processor (AP).
    pub ap_entry: GuestPhysAddr,
}

/// Ramdisk image information.
#[derive(Debug, Default, Clone)]
pub struct RamdiskInfo {
    /// The load address in GPA for the ramdisk image.
    pub load_gpa: GuestPhysAddr,
    /// The size in bytes of the ramdisk image, `None` if not known yet.
    pub size: Option<usize>,
}

/// A part of `AxVMConfig`, which stores configuration attributes related to the load address of VM images.
#[derive(Debug, Default, Clone)]
pub struct VMImageConfig {
    /// The load address in GPA for the kernel image.
    pub kernel_load_gpa: GuestPhysAddr,
    /// Whether VM images are loaded from the host filesystem.
    pub loaded_from_filesystem: bool,
    /// The load address in GPA for the BIOS image, `None` if not used.
    pub bios_load_gpa: Option<GuestPhysAddr>,
    /// The load address in GPA for the device tree blob (DTB), `None` if not used.
    pub dtb_load_gpa: Option<GuestPhysAddr>,
    /// Ramdisk image info, `None` if not used.
    pub ramdisk: Option<RamdiskInfo>,
}

/// Physical interrupt source forwarded through a guest's virtual controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassthroughInterrupt {
    /// Architecture-local physical interrupt source number.
    pub source: u32,
    /// Trigger mode declared by firmware for the physical device.
    pub trigger: InterruptTriggerMode,
}

/// Runtime configuration for one VM.
#[derive(Debug)]
pub struct AxVMConfig {
    id: usize,
    name: String,
    pub(crate) phys_cpu_ls: PhysCpuList,
    /// vCPU configuration.
    pub cpu_config: AxVCpuConfig,
    /// VM image configuration.
    pub image_config: VMImageConfig,
    emu_devices: Vec<EmulatedDeviceConfig>,
    pass_through_devices: Vec<PassThroughDeviceConfig>,
    excluded_devices: Vec<Vec<String>>,
    pass_through_addresses: Vec<PassThroughAddressConfig>,
    reserved_address_ranges: Vec<ReservedAddressConfig>,
    pass_through_ports: Vec<PassThroughPortConfig>,
    address_space_policy: AddressSpacePolicy,
    memory_regions: Vec<VmMemConfig>,
    boot_policy: GuestBootPolicy,
    // Physical interrupt sources forwarded to the guest in passthrough mode.
    passthrough_irq_list: Vec<PassthroughInterrupt>,
    interrupt_mode: VMInterruptMode,
    serial_profile: GuestSerialProfile,
    serial_fdt_identity: Option<GuestSerialFdtIdentity>,
    gic_profile: Option<GuestGicProfile>,
    plic_profile: Option<GuestPlicProfile>,
    timer_profile: Option<GuestTimerProfile>,
    serial_backend_factory: Arc<dyn SerialBackendFactory>,
}

/// Parameters used to build an [`AxVMConfig`].
#[derive(Debug, Default)]
pub struct AxVMConfigParams {
    pub id: usize,
    pub name: String,
    pub phys_cpu_ls: PhysCpuList,
    pub cpu_config: AxVCpuConfig,
    pub image_config: VMImageConfig,
    pub emu_devices: Vec<EmulatedDeviceConfig>,
    pub pass_through_devices: Vec<PassThroughDeviceConfig>,
    pub excluded_devices: Vec<Vec<String>>,
    pub pass_through_addresses: Vec<PassThroughAddressConfig>,
    pub reserved_address_ranges: Vec<ReservedAddressConfig>,
    pub pass_through_ports: Vec<PassThroughPortConfig>,
    pub address_space_policy: AddressSpacePolicy,
    pub memory_regions: Vec<VmMemConfig>,
    pub boot_policy: GuestBootPolicy,
    pub interrupt_mode: VMInterruptMode,
    /// Machine-owned virtual serial resources.
    pub serial_profile: Option<GuestSerialProfile>,
    /// App-owned backend factory for the mandatory virtual serial device.
    pub serial_backend_factory: Option<Arc<dyn SerialBackendFactory>>,
}

impl AxVMConfig {
    pub fn new(params: AxVMConfigParams) -> Self {
        let machine = crate::machine::current_machine_profile(params.phys_cpu_ls.cpu_num());
        let serial_profile = params.serial_profile.unwrap_or(machine.serial);
        Self {
            id: params.id,
            name: params.name,
            phys_cpu_ls: params.phys_cpu_ls,
            cpu_config: params.cpu_config,
            image_config: params.image_config,
            emu_devices: params.emu_devices,
            pass_through_devices: params.pass_through_devices,
            excluded_devices: params.excluded_devices,
            pass_through_addresses: params.pass_through_addresses,
            reserved_address_ranges: params.reserved_address_ranges,
            pass_through_ports: params.pass_through_ports,
            address_space_policy: params.address_space_policy,
            memory_regions: params.memory_regions,
            boot_policy: params.boot_policy,
            passthrough_irq_list: Vec::new(),
            interrupt_mode: params.interrupt_mode,
            serial_profile,
            serial_fdt_identity: None,
            gic_profile: None,
            plic_profile: None,
            timer_profile: machine.timer,
            serial_backend_factory: params
                .serial_backend_factory
                .unwrap_or_else(|| Arc::new(NullSerialBackendFactory)),
        }
    }

    #[cfg(test)]
    pub(crate) fn default_for_test(id: usize, name: &str) -> Self {
        Self::new(AxVMConfigParams {
            id,
            name: String::from(name),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            ..Default::default()
        })
    }

    /// Returns VM id.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Returns VM name.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Returns configurations related to VM image load addresses.
    pub fn image_config(&self) -> &VMImageConfig {
        &self.image_config
    }

    /// Clears the configured DTB load address when no guest DTB is available.
    pub fn clear_dtb_load_gpa(&mut self) {
        self.image_config.dtb_load_gpa = None;
    }

    /// Sets the DTB load address used as an architecture boot argument.
    pub fn set_dtb_load_gpa(&mut self, dtb_load_gpa: GuestPhysAddr) {
        self.image_config.dtb_load_gpa = Some(dtb_load_gpa);
    }

    /// Returns whether VM images are loaded from the host filesystem.
    pub fn images_loaded_from_filesystem(&self) -> bool {
        self.image_config.loaded_from_filesystem
    }

    /// Returns the entry address in GPA for the Bootstrap Processor (BSP).
    pub fn bsp_entry(&self) -> GuestPhysAddr {
        // Retrieves BSP entry from the CPU configuration.
        self.cpu_config.bsp_entry
    }

    /// Returns the entry address in GPA for the Application Processor (AP).
    pub fn ap_entry(&self) -> GuestPhysAddr {
        // Retrieves AP entry from the CPU configuration.
        self.cpu_config.ap_entry
    }

    /// Returns a mutable reference to the physical CPU list.
    pub fn phys_cpu_ls_mut(&mut self) -> &mut PhysCpuList {
        &mut self.phys_cpu_ls
    }

    /// Returns the list of excluded devices.
    pub fn excluded_devices(&self) -> &Vec<Vec<String>> {
        &self.excluded_devices
    }

    /// Adds one physical-device path to the passthrough exclusion set.
    pub fn exclude_device_path(&mut self, path: String) {
        if !self
            .excluded_devices
            .iter()
            .flatten()
            .any(|excluded| excluded == &path)
        {
            self.excluded_devices.push(alloc::vec![path]);
        }
    }

    /// Returns the list of passthrough address configurations.
    pub fn pass_through_addresses(&self) -> &Vec<PassThroughAddressConfig> {
        &self.pass_through_addresses
    }

    /// Returns guest address ranges reserved from default passthrough mapping.
    pub fn reserved_address_ranges(&self) -> &Vec<ReservedAddressConfig> {
        &self.reserved_address_ranges
    }

    /// Adds a guest address range reserved from default passthrough mapping.
    pub fn add_reserved_address_range(&mut self, range: ReservedAddressConfig) {
        self.reserved_address_ranges.push(range);
    }

    /// Returns the list of passthrough host I/O port configurations.
    pub fn pass_through_ports(&self) -> &Vec<PassThroughPortConfig> {
        &self.pass_through_ports
    }

    /// Returns the guest physical address space population policy.
    pub fn address_space_policy(&self) -> AddressSpacePolicy {
        self.address_space_policy
    }

    /// Returns configurations related to VM memory regions.
    pub fn memory_regions(&self) -> &[VmMemConfig] {
        &self.memory_regions
    }

    /// Replaces configurations related to VM memory regions.
    pub fn set_memory_regions(&mut self, memory_regions: Vec<VmMemConfig>) {
        self.memory_regions = memory_regions;
    }

    /// Returns the policy used to adjust runtime boot image addresses.
    pub fn boot_policy(&self) -> GuestBootPolicy {
        self.boot_policy
    }

    /// Sets the policy used to adjust runtime boot image addresses.
    pub fn set_boot_policy(&mut self, boot_policy: GuestBootPolicy) {
        self.boot_policy = boot_policy;
    }

    /// Returns configurations related to VM emulated devices.
    pub fn emu_devices(&self) -> &Vec<EmulatedDeviceConfig> {
        &self.emu_devices
    }

    /// Returns configurations related to VM passthrough devices.
    pub fn pass_through_devices(&self) -> &Vec<PassThroughDeviceConfig> {
        &self.pass_through_devices
    }

    /// Adds a new passthrough device to the VM configuration.
    pub fn add_pass_through_device(&mut self, device: PassThroughDeviceConfig) {
        self.pass_through_devices.push(device);
    }

    /// Removes passthrough device from the VM configuration.
    pub fn remove_pass_through_device(&mut self, device: PassThroughDeviceConfig) {
        self.pass_through_devices.retain(|d| d != &device);
    }

    /// Clears all passthrough devices from the VM configuration.
    pub fn clear_pass_through_devices(&mut self) {
        self.pass_through_devices.clear();
    }

    /// Adds a physical interrupt source forwarded to the guest.
    pub fn add_pass_through_irq(&mut self, source: u32, trigger: InterruptTriggerMode) {
        let route = PassthroughInterrupt { source, trigger };
        if let Some(existing) = self
            .passthrough_irq_list
            .iter_mut()
            .find(|existing| existing.source == source)
        {
            *existing = route;
        } else {
            self.passthrough_irq_list.push(route);
        }
    }

    /// Returns the physical interrupt sources forwarded to the guest.
    pub fn pass_through_irqs(&self) -> &[PassthroughInterrupt] {
        &self.passthrough_irq_list
    }

    /// Returns the interrupt mode of the VM.
    pub fn interrupt_mode(&self) -> VMInterruptMode {
        self.interrupt_mode
    }

    /// Returns the machine-owned virtual serial resources.
    pub(crate) const fn serial_profile(&self) -> GuestSerialProfile {
        self.serial_profile
    }

    /// Replaces the machine serial resources and its bus descriptor atomically.
    pub fn replace_machine_serial(
        &mut self,
        profile: GuestSerialProfile,
        identity: Option<GuestSerialFdtIdentity>,
    ) -> crate::AxVmResult {
        let mut serial_index = None;
        for (index, device) in self.emu_devices.iter().enumerate() {
            if device.emu_type != EmulatedDeviceType::Console {
                continue;
            }
            if serial_index.replace(index).is_some() {
                return Err(crate::AxVmError::invalid_config(
                    "machine profile has more than one serial device",
                ));
            }
        }
        let serial_index = serial_index.ok_or_else(|| {
            crate::AxVmError::invalid_config("machine profile has no serial device")
        })?;

        self.emu_devices[serial_index] = crate::machine::serial_device_config(profile);
        self.serial_profile = profile;
        self.serial_fdt_identity = identity;
        Ok(())
    }

    /// Returns firmware identity retained for the virtual serial node.
    pub fn serial_fdt_identity(&self) -> Option<&GuestSerialFdtIdentity> {
        self.serial_fdt_identity.as_ref()
    }

    /// Replaces the virtual GIC windows with host firmware resources.
    pub fn replace_machine_gic(&mut self, mut profile: GuestGicProfile) -> crate::AxVmResult {
        let cpu_num = self.phys_cpu_ls.cpu_num().max(1);
        let (cpu_region, cpu_region_name, cpu_count) = match profile.cpu_region {
            GuestGicCpuRegion::CpuInterface(mut region) => {
                const GICV2_DISTRIBUTOR_SIZE: usize = 0x1_000;
                const GICV2_CPU_INTERFACE_SIZE: usize = 0x2_000;
                if profile.distributor.length < GICV2_DISTRIBUTOR_SIZE {
                    return Err(crate::AxVmError::invalid_config(alloc::format!(
                        "AArch64 GICv2 distributor window {:#x} is smaller than \
                         {GICV2_DISTRIBUTOR_SIZE:#x}",
                        profile.distributor.length
                    )));
                }
                if region.length < GICV2_CPU_INTERFACE_SIZE {
                    return Err(crate::AxVmError::invalid_config(alloc::format!(
                        "AArch64 GICv2 CPU-interface window {:#x} is smaller than \
                         {GICV2_CPU_INTERFACE_SIZE:#x}",
                        region.length
                    )));
                }
                profile.distributor.length = GICV2_DISTRIBUTOR_SIZE;
                region.length = GICV2_CPU_INTERFACE_SIZE;
                profile.cpu_region = GuestGicCpuRegion::CpuInterface(region);
                (region, "gic-cpu-interface", None)
            }
            GuestGicCpuRegion::Redistributors(region) => {
                const GICV3_DISTRIBUTOR_MINIMUM_SIZE: usize = 0x1_0000;
                if profile.distributor.length < GICV3_DISTRIBUTOR_MINIMUM_SIZE {
                    return Err(crate::AxVmError::invalid_config(alloc::format!(
                        "AArch64 GICv3 distributor window {:#x} is smaller than \
                         {GICV3_DISTRIBUTOR_MINIMUM_SIZE:#x}",
                        profile.distributor.length
                    )));
                }
                let required_size = cpu_num
                    .checked_mul(crate::machine::AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE)
                    .ok_or_else(|| {
                        crate::AxVmError::invalid_config(
                            "AArch64 redistributor window size overflows usize",
                        )
                    })?;
                if region.length < required_size {
                    return Err(crate::AxVmError::invalid_config(alloc::format!(
                        "AArch64 GIC redistributor window {:#x} is smaller than required size \
                         {required_size:#x}",
                        region.length
                    )));
                }
                (region, "gic-redistributors", Some(cpu_num))
            }
        };

        let distributor = self
            .emu_devices
            .iter_mut()
            .find(|device| device.emu_type == EmulatedDeviceType::InterruptController)
            .ok_or_else(|| {
                crate::AxVmError::invalid_config(
                    "AArch64 machine profile has no interrupt controller",
                )
            })?;
        distributor.base_gpa = profile.distributor.base;
        distributor.length = profile.distributor.length;

        let per_cpu = self
            .emu_devices
            .iter_mut()
            .find(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion)
            .ok_or_else(|| {
                crate::AxVmError::invalid_config(
                    "AArch64 machine profile has no per-CPU GIC region",
                )
            })?;
        per_cpu.name = cpu_region_name.into();
        per_cpu.base_gpa = cpu_region.base;
        per_cpu.length = cpu_region.length;
        per_cpu.cfg_list.clear();
        if let Some(cpu_count) = cpu_count {
            per_cpu.cfg_list.push(cpu_count);
        }

        self.gic_profile = Some(profile);
        Ok(())
    }

    /// Returns host firmware resources retained by the virtual GIC.
    pub fn gic_profile(&self) -> Option<&GuestGicProfile> {
        self.gic_profile.as_ref()
    }

    /// Replaces the AArch64 architectural timer resources with validated host firmware data.
    pub fn replace_machine_timer(&mut self, profile: GuestTimerProfile) -> crate::AxVmResult {
        if self.timer_profile.is_none() {
            return Err(crate::AxVmError::invalid_config(
                "the selected machine has no AArch64 architectural timer",
            ));
        }
        profile
            .validated_intids()
            .map_err(crate::AxVmError::invalid_config)?;
        self.timer_profile = Some(profile);
        Ok(())
    }

    /// Returns the machine-owned AArch64 architectural timer resources.
    pub fn timer_profile(&self) -> Option<&GuestTimerProfile> {
        self.timer_profile.as_ref()
    }

    /// Replaces the virtual PLIC window with host firmware resources.
    pub fn replace_machine_plic(&mut self, profile: GuestPlicProfile) -> crate::AxVmResult {
        let plic = self
            .emu_devices
            .iter_mut()
            .find(|device| device.emu_type == EmulatedDeviceType::PPPTGlobal)
            .ok_or_else(|| {
                crate::AxVmError::invalid_config("RISC-V machine profile has no PLIC controller")
            })?;
        let [contexts] = plic.cfg_list.as_slice() else {
            return Err(crate::AxVmError::invalid_config(
                "RISC-V PLIC profile has no context count",
            ));
        };
        const CONTEXT_CONTROL_OFFSET: usize = 0x20_0000;
        const CONTEXT_STRIDE: usize = 0x1000;
        const CLAIM_COMPLETE_SIZE: usize = 8;
        let minimum_length = contexts
            .checked_mul(CONTEXT_STRIDE)
            .and_then(|offset| offset.checked_add(CONTEXT_CONTROL_OFFSET))
            .and_then(|offset| offset.checked_add(CLAIM_COMPLETE_SIZE))
            .ok_or_else(|| {
                crate::AxVmError::invalid_config("RISC-V PLIC context window size overflows usize")
            })?;
        if profile.length < minimum_length {
            return Err(crate::AxVmError::invalid_config(alloc::format!(
                "RISC-V PLIC window {:#x} is smaller than required size {minimum_length:#x}",
                profile.length
            )));
        }

        plic.base_gpa = profile.base;
        plic.length = profile.length;
        self.plic_profile = Some(profile);
        Ok(())
    }

    /// Returns host firmware resources retained by the virtual PLIC.
    pub fn plic_profile(&self) -> Option<&GuestPlicProfile> {
        self.plic_profile.as_ref()
    }

    /// Returns the factory that creates a backend for each virtual UART graph.
    pub fn serial_backend_factory(&self) -> Arc<dyn SerialBackendFactory> {
        self.serial_backend_factory.clone()
    }

    /// Relocate the guest kernel image while preserving the configured
    /// entry-point offsets relative to the load address.
    pub fn relocate_kernel_image(&mut self, kernel_load_gpa: GuestPhysAddr) {
        let old_load = self.image_config.kernel_load_gpa.as_usize();
        let new_load = kernel_load_gpa.as_usize();

        let bsp_offset = self
            .cpu_config
            .bsp_entry
            .as_usize()
            .checked_sub(old_load)
            .expect("BSP entry must not be below kernel load address");
        let ap_offset = self
            .cpu_config
            .ap_entry
            .as_usize()
            .checked_sub(old_load)
            .expect("AP entry must not be below kernel load address");

        self.image_config.kernel_load_gpa = kernel_load_gpa;
        self.cpu_config.bsp_entry = GuestPhysAddr::from(new_load + bsp_offset);
        self.cpu_config.ap_entry = GuestPhysAddr::from(new_load + ap_offset);
    }
}

impl Default for AxVMConfig {
    fn default() -> Self {
        Self::new(AxVMConfigParams::default())
    }
}

/// Represents the list of physical CPUs available for the VM.
#[derive(Debug, Default, Clone)]
pub struct PhysCpuList {
    cpu_num: usize,
    phys_cpu_ids: Option<Vec<usize>>,
    phys_cpu_sets: Option<Vec<usize>>,
}

impl PhysCpuList {
    /// Creates a physical CPU list.
    pub fn new(
        cpu_num: usize,
        phys_cpu_ids: Option<Vec<usize>>,
        phys_cpu_sets: Option<Vec<usize>>,
    ) -> Self {
        Self {
            cpu_num,
            phys_cpu_ids,
            phys_cpu_sets,
        }
    }

    /// Returns vCpu id list and its corresponding pCpu affinity list, as well as its physical id.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    /// if the pCPU id is not provided, the vCpu's physical id will be set as vCpu id.
    ///
    /// Returns a vector of tuples, each tuple contains:
    /// - The vCpu id.
    /// - The pCpu affinity mask, `None` if not set.
    /// - The physical id of the vCpu, equal to vCpu id if not provided.
    pub fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        if let Some(phys_cpu_ids) = &self.phys_cpu_ids
            && self.cpu_num != phys_cpu_ids.len()
        {
            error!(
                "ERROR!!!: cpu_num: {}, phys_cpu_ids: {:?}",
                self.cpu_num, self.phys_cpu_ids
            );
        }
        CurrentArch::vcpu_affinities(
            self.cpu_num,
            self.phys_cpu_ids.as_deref(),
            self.phys_cpu_sets.as_deref(),
        )
    }

    /// Returns the number of CPUs.
    pub fn cpu_num(&self) -> usize {
        self.cpu_num
    }

    /// Returns the physical CPU IDs.
    pub fn phys_cpu_ids(&self) -> &Option<Vec<usize>> {
        &self.phys_cpu_ids
    }

    /// Returns the physical CPU sets.
    pub fn phys_cpu_sets(&self) -> &Option<Vec<usize>> {
        &self.phys_cpu_sets
    }

    /// Sets the guest CPU sets.
    pub fn set_guest_cpu_sets(&mut self, phys_cpu_sets: Vec<usize>) {
        self.phys_cpu_sets = Some(phys_cpu_sets);
    }

    /// Sets the CPU IDs exposed to the guest.
    pub fn set_guest_phys_cpu_ids(&mut self, phys_cpu_ids: Vec<usize>) {
        self.phys_cpu_ids = Some(phys_cpu_ids);
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn memory_region(gpa: usize, size: usize, map_type: VmMemMappingType) -> VmMemConfig {
        VmMemConfig {
            gpa,
            size,
            flags: 0x7,
            map_type,
        }
    }

    #[test]
    fn set_memory_regions_replaces_stale_snapshot_after_config_enrichment() {
        let main_memory = memory_region(0x8000_0000, 0x200000, VmMemMappingType::MapIdentical);
        let reserved_memory = memory_region(0x110000, 0x10000, VmMemMappingType::MapReserved);
        let mut config = AxVMConfig::default_for_test(1, "linux");

        config.set_memory_regions(vec![main_memory.clone()]);
        assert_eq!(config.memory_regions().len(), 1);

        config.set_memory_regions(vec![main_memory, reserved_memory]);

        let regions = config.memory_regions();
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[1].gpa, 0x110000);
        assert_eq!(regions[1].size, 0x10000);
        assert_eq!(regions[1].map_type, VmMemMappingType::MapReserved);
    }

    #[test]
    fn replacing_machine_serial_updates_the_internal_bus_descriptor() {
        let machine =
            crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Aarch64, 1);
        let mut config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            emu_devices: machine.emulated_devices,
            serial_profile: Some(machine.serial),
            ..Default::default()
        });
        let profile = GuestSerialProfile {
            model: crate::machine::GuestSerialModel::Uart16550,
            transport: crate::machine::GuestSerialTransport::Mmio {
                base: 0xfeb5_0000,
                length: 0x100,
                register_shift: 2,
                register_width: axdevice_base::AccessWidth::Dword,
            },
            irq: 33,
            clock_hz: 24_000_000,
        };
        let identity = GuestSerialFdtIdentity {
            node_path: "/serial@feb50000".into(),
            node_phandle: Some(0x2d1),
            interrupt_parent: 1,
            interrupt_specifier: vec![0, 0x14d, 4],
            stdout_path: "/serial@feb50000:1500000".into(),
            clock_references: Vec::new(),
        };

        config
            .replace_machine_serial(profile, Some(identity.clone()))
            .unwrap();

        assert_eq!(config.serial_profile(), profile);
        assert_eq!(config.serial_fdt_identity(), Some(&identity));
        let descriptor = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::Console)
            .unwrap();
        assert_eq!(descriptor.name, "uart");
        assert_eq!(descriptor.base_gpa, 0xfeb5_0000);
        assert_eq!(descriptor.length, 0x100);
        assert_eq!(descriptor.irq_id, 33);
        assert_eq!(descriptor.cfg_list, [24_000_000, 2, 4]);
    }

    #[test]
    fn replacing_machine_gic_updates_both_trapped_windows() {
        let machine =
            crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Aarch64, 1);
        let mut config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            emu_devices: machine.emulated_devices,
            serial_profile: Some(machine.serial),
            ..Default::default()
        });
        let profile = GuestGicProfile {
            compatible: "arm,gic-v3".into(),
            node_path: "/interrupt-controller@fe600000".into(),
            node_phandle: Some(1),
            distributor: crate::machine::GuestMmioRegion {
                base: 0xfe60_0000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::Redistributors(crate::machine::GuestMmioRegion {
                base: 0xfe68_0000,
                length: 0x10_0000,
            }),
        };

        config.replace_machine_gic(profile.clone()).unwrap();

        assert_eq!(config.gic_profile(), Some(&profile));
        let distributor = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::InterruptController)
            .unwrap();
        assert_eq!(
            (distributor.base_gpa, distributor.length),
            (0xfe60_0000, 0x1_0000)
        );
        let redistributor = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion)
            .unwrap();
        assert_eq!(
            (redistributor.base_gpa, redistributor.length),
            (0xfe68_0000, 0x10_0000)
        );
    }

    #[test]
    fn replacing_machine_gicv2_normalizes_overlapping_firmware_windows() {
        let machine =
            crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Aarch64, 1);
        let mut config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            emu_devices: machine.emulated_devices,
            serial_profile: Some(machine.serial),
            ..Default::default()
        });
        let profile = GuestGicProfile {
            compatible: "arm,gic-400".into(),
            node_path: "/interrupt-controller@2a701000".into(),
            node_phandle: Some(1),
            distributor: crate::machine::GuestMmioRegion {
                base: 0x2a70_1000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::CpuInterface(crate::machine::GuestMmioRegion {
                base: 0x2a70_2000,
                length: 0x1_0000,
            }),
        };

        config.replace_machine_gic(profile).unwrap();

        let normalized = config.gic_profile().unwrap();
        assert_eq!(normalized.distributor.length, 0x1_000);
        assert_eq!(
            normalized.cpu_region,
            GuestGicCpuRegion::CpuInterface(crate::machine::GuestMmioRegion {
                base: 0x2a70_2000,
                length: 0x2_000,
            })
        );
        let distributor = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::InterruptController)
            .unwrap();
        let cpu_interface = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion)
            .unwrap();
        assert_eq!(
            (distributor.base_gpa, distributor.length),
            (0x2a70_1000, 0x1_000)
        );
        assert_eq!(
            (cpu_interface.base_gpa, cpu_interface.length),
            (0x2a70_2000, 0x2_000)
        );
    }

    #[test]
    fn replacing_machine_plic_updates_the_trapped_window() {
        let machine =
            crate::machine::machine_profile_for(crate::machine::MachineArchitecture::Riscv64, 1);
        let mut config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            emu_devices: machine.emulated_devices,
            serial_profile: Some(machine.serial),
            ..Default::default()
        });
        let profile = GuestPlicProfile {
            node_path: "/soc/plic@d000000".into(),
            node_phandle: Some(9),
            base: 0x0d00_0000,
            length: 0x80_0000,
        };

        config.replace_machine_plic(profile.clone()).unwrap();

        assert_eq!(config.plic_profile(), Some(&profile));
        let plic = config
            .emu_devices()
            .iter()
            .find(|device| device.emu_type == EmulatedDeviceType::PPPTGlobal)
            .unwrap();
        assert_eq!((plic.base_gpa, plic.length), (0x0d00_0000, 0x80_0000));
    }
}
