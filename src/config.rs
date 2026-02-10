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

//! The configuration structure for the VM.
//! The `AxVMCrateConfig` is generated from toml file, and then converted to `AxVMConfig` for the VM creation.

use alloc::string::String;
use alloc::vec::Vec;

use axaddrspace::GuestPhysAddr;

pub use axvmconfig::{
    AxVMCrateConfig, EmulatedDeviceConfig, PassThroughAddressConfig, PassThroughDeviceConfig,
    VMInterruptMode, VMType, VmMemConfig, VmMemMappingType,
};

// /// A part of `AxVCpuConfig`, which represents an architecture-dependent `VCpu`.
// ///
// /// The concrete type of configuration is defined in `AxArchVCpuImpl`.
// #[derive(Clone, Copy, Debug, Default)]
// pub struct AxArchVCpuConfig<H: AxVMHal> {
//     pub create_config: <AxArchVCpuImpl<H> as AxArchVCpu>::CreateConfig,
//     pub setup_config: <AxArchVCpuImpl<H> as AxArchVCpu>::SetupConfig,
// }
/// A part of `AxVMConfig`, which represents a `VCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVCpuConfig {
    // pub arch_config: AxArchVCpuConfig,
    /// The entry address in GPA for the Bootstrap Processor (BSP).
    pub bsp_entry: GuestPhysAddr,
    /// The entry address in GPA for the Application Processor (AP).
    pub ap_entry: GuestPhysAddr,
}

/// A part of `AxVMConfig`, which stores configuration attributes related to the load address of VM images.
#[derive(Debug, Default, Clone)]
pub struct VMImageConfig {
    /// The load address in GPA for the kernel image.
    pub kernel_load_gpa: GuestPhysAddr,
    /// The load address in GPA for the BIOS image, `None` if not used.
    pub bios_load_gpa: Option<GuestPhysAddr>,
    /// The load address in GPA for the device tree blob (DTB), `None` if not used.
    pub dtb_load_gpa: Option<GuestPhysAddr>,
    /// The load address in GPA for the ramdisk image, `None` if not used.
    pub ramdisk_load_gpa: Option<GuestPhysAddr>,
}

/// A part of `AxVMCrateConfig`, which represents a `VM`.
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
    // TODO: improve interrupt passthrough
    spi_list: Vec<u32>,
    interrupt_mode: VMInterruptMode,
}

impl From<AxVMCrateConfig> for AxVMConfig {
    fn from(cfg: AxVMCrateConfig) -> Self {
        Self {
            id: cfg.base.id,
            name: cfg.base.name,
            vm_type: VMType::from(cfg.base.vm_type),
            phys_cpu_ls: PhysCpuList {
                cpu_num: cfg.base.cpu_num,
                phys_cpu_ids: cfg.base.phys_cpu_ids,
                phys_cpu_sets: cfg.base.phys_cpu_sets,
            },
            cpu_config: AxVCpuConfig {
                bsp_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
                ap_entry: GuestPhysAddr::from(cfg.kernel.entry_point),
            },
            image_config: VMImageConfig {
                kernel_load_gpa: GuestPhysAddr::from(cfg.kernel.kernel_load_addr),
                bios_load_gpa: cfg.kernel.bios_load_addr.map(GuestPhysAddr::from),
                dtb_load_gpa: cfg.kernel.dtb_load_addr.map(GuestPhysAddr::from),
                ramdisk_load_gpa: cfg.kernel.ramdisk_load_addr.map(GuestPhysAddr::from),
            },
            // memory_regions: cfg.kernel.memory_regions,
            emu_devices: cfg.devices.emu_devices,
            pass_through_devices: cfg.devices.passthrough_devices,
            excluded_devices: cfg.devices.excluded_devices,
            pass_through_addresses: cfg.devices.passthrough_addresses,
            spi_list: Vec::new(),
            interrupt_mode: cfg.devices.interrupt_mode,
        }
    }
}

impl AxVMConfig {
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
    // /// Returns configurations related to VM memory regions.
    // pub fn memory_regions(&self) -> Vec<VmMemConfig> {
    //     &self.memory_regions
    // }

    // /// Adds a new memory region to the VM configuration.
    // pub fn add_memory_region(&mut self, region: VmMemConfig) {
    //     self.memory_regions.push(region);
    // }

    // /// Checks if the VM memory regions contain a specific range.
    // pub fn contains_memory_range(&self, range: &Range<usize>) -> bool {
    //     self.memory_regions
    //         .iter()
    //         .any(|region| region.gpa <= range.start && region.gpa + region.size >= range.end)
    // }

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
        self.pass_through_devices.retain(|d| d == &device);
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
}

/// Represents the list of physical CPUs available for the VM.
#[derive(Debug, Default, Clone)]
pub struct PhysCpuList {
    cpu_num: usize,
    phys_cpu_ids: Option<Vec<usize>>,
    phys_cpu_sets: Option<Vec<usize>>,
}

impl PhysCpuList {
    /// Returns vCpu id list and its corresponding pCpu affinity list, as well as its physical id.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    /// if the pCPU id is not provided, the vCpu's physical id will be set as vCpu id.
    ///
    /// Returns a vector of tuples, each tuple contains:
    /// - The vCpu id.
    /// - The pCpu affinity mask, `None` if not set.
    /// - The physical id of the vCpu, equal to vCpu id if not provided.
    pub fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        let mut vcpu_pcpu_tuples = Vec::new();
        #[cfg(target_arch = "riscv64")]
        let mut pcpu_mask_flag = false;

        if let Some(phys_cpu_ids) = &self.phys_cpu_ids
            && self.cpu_num != phys_cpu_ids.len()
        {
            error!(
                "ERROR!!!: cpu_num: {}, phys_cpu_ids: {:?}",
                self.cpu_num, self.phys_cpu_ids
            );
        }

        for vcpu_id in 0..self.cpu_num {
            vcpu_pcpu_tuples.push((vcpu_id, None, vcpu_id));
        }

        #[cfg(target_arch = "riscv64")]
        if let Some(phys_cpu_sets) = &self.phys_cpu_sets {
            pcpu_mask_flag = true;
            for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
                vcpu_pcpu_tuples[vcpu_id].1 = Some(*pcpu_mask_bitmap);
            }
        }

        #[cfg(not(target_arch = "riscv64"))]
        if let Some(phys_cpu_sets) = &self.phys_cpu_sets {
            for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
                vcpu_pcpu_tuples[vcpu_id].1 = Some(*pcpu_mask_bitmap);
            }
        }

        if let Some(phys_cpu_ids) = &self.phys_cpu_ids {
            for (vcpu_id, phys_id) in phys_cpu_ids.iter().enumerate() {
                vcpu_pcpu_tuples[vcpu_id].2 = *phys_id;
                #[cfg(target_arch = "riscv64")]
                {
                    if !pcpu_mask_flag {
                        // if don't assign pcpu mask yet, assign it manually
                        vcpu_pcpu_tuples[vcpu_id].1 = Some(1 << (*phys_id));
                    }
                }
            }
        }
        vcpu_pcpu_tuples
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
}
