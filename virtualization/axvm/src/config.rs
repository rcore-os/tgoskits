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

use alloc::{string::String, vec::Vec};

pub use axvm_types::{
    AddressSpacePolicy, EmulatedDeviceConfig, GuestPhysAddr, PassThroughAddressConfig,
    PassThroughDeviceConfig, PassThroughPortConfig, ReservedAddressConfig, VMBootProtocol,
    VMInterruptMode, VMType, VmMemConfig, VmMemMappingType,
};

use crate::arch::{ArchOps, CurrentArch};

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

/// Runtime configuration for one VM.
#[derive(Debug, Default)]
pub struct AxVMConfig {
    id: usize,
    name: String,
    #[allow(dead_code)]
    vm_type: VMType,
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
    // TODO: improve interrupt passthrough
    spi_list: Vec<u32>,
    interrupt_mode: VMInterruptMode,
}

/// Parameters used to build an [`AxVMConfig`].
#[derive(Debug, Default)]
pub struct AxVMConfigParams {
    pub id: usize,
    pub name: String,
    pub vm_type: VMType,
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
}

impl AxVMConfig {
    pub fn new(params: AxVMConfigParams) -> Self {
        Self {
            id: params.id,
            name: params.name,
            vm_type: params.vm_type,
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
            spi_list: Vec::new(),
            interrupt_mode: params.interrupt_mode,
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

    /// Adds a passthrough SPI to the VM configuration.
    pub fn add_pass_through_spi(&mut self, spi: u32) {
        self.spi_list.push(spi);
    }

    /// Returns the list of passthrough SPIs.
    pub fn pass_through_spis(&self) -> &Vec<u32> {
        &self.spi_list
    }

    /// Returns the interrupt mode of the VM.
    pub fn interrupt_mode(&self) -> VMInterruptMode {
        self.interrupt_mode
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
}
