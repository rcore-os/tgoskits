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

use alloc::{string::String, vec::Vec};

use axaddrspace::GuestPhysAddr;
pub use axvmconfig::{
    AxVMCrateConfig, EmulatedDeviceConfig, EmulatedDeviceType, PassThroughAddressConfig,
    PassThroughDeviceConfig, VMBootProtocol, VMInterruptMode, VMType, VmMemConfig,
    VmMemMappingType,
};

use crate::VMMemoryRegion;

const BIOS_RESERVED_SIZE: usize = 2 * 1024 * 1024;

/// Default BIOS load GPA for x86_64 built-in BIOS.
#[cfg(target_arch = "x86_64")]
const DEFAULT_X86_BIOS_LOAD_GPA: usize = 0x8000;

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
        let bios_load_gpa = configured_bios_load_gpa(&cfg);
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
                loaded_from_filesystem: cfg.kernel.image_location.as_deref() == Some("fs"),
                bios_load_gpa,
                dtb_load_gpa: cfg.kernel.dtb_load_addr.map(GuestPhysAddr::from),
                ramdisk: cfg.kernel.ramdisk_load_addr.map(|addr| RamdiskInfo {
                    load_gpa: GuestPhysAddr::from(addr),
                    size: None,
                }),
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

fn configured_bios_load_gpa(cfg: &AxVMCrateConfig) -> Option<GuestPhysAddr> {
    if !cfg.kernel.enable_bios {
        return None;
    }

    if let Some(addr) = cfg.kernel.bios_load_addr {
        return Some(GuestPhysAddr::from(addr));
    }

    #[cfg(target_arch = "x86_64")]
    if cfg.kernel.boot_firmware_path().is_none()
        && cfg.kernel.effective_boot_protocol() == VMBootProtocol::Multiboot
    {
        return Some(GuestPhysAddr::from(DEFAULT_X86_BIOS_LOAD_GPA));
    }

    None
}

pub fn adjusted_kernel_load_gpa(
    main_memory: &VMMemoryRegion,
    boot_protocol: VMBootProtocol,
    bios_load_gpa: Option<GuestPhysAddr>,
) -> Option<GuestPhysAddr> {
    if !main_memory.is_identical() {
        return None;
    }

    let mut kernel_addr = main_memory.gpa;
    if boot_protocol == VMBootProtocol::Multiboot && bios_load_gpa.is_some() {
        kernel_addr += BIOS_RESERVED_SIZE;
    }
    Some(kernel_addr)
}

impl AxVMConfig {
    /// Creates a VM configuration whose guest resources are supplied by a host control API.
    pub fn new_host_controlled(id: usize, name: String, vcpu_count: usize) -> Self {
        Self {
            id,
            name,
            vm_type: VMType::VMTLinux,
            phys_cpu_ls: PhysCpuList {
                cpu_num: vcpu_count,
                phys_cpu_ids: None,
                phys_cpu_sets: None,
            },
            cpu_config: AxVCpuConfig::default(),
            image_config: VMImageConfig::default(),
            emu_devices: host_controlled_emu_devices(),
            pass_through_devices: Vec::new(),
            excluded_devices: Vec::new(),
            pass_through_addresses: host_controlled_passthrough_addresses(),
            spi_list: Vec::new(),
            interrupt_mode: VMInterruptMode::NoIrq,
        }
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

    /// Returns the guest CPU topology and optional host affinity mapping.
    pub fn vcpu_topology(&self) -> Vec<(usize, Option<usize>, usize)> {
        self.phys_cpu_ls.get_vcpu_affinities_pcpu_ids()
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

#[cfg(target_arch = "x86_64")]
fn host_controlled_emu_devices() -> Vec<EmulatedDeviceConfig> {
    alloc::vec![
        EmulatedDeviceConfig {
            name: "kvm-com1".into(),
            emu_type: EmulatedDeviceType::Console,
            ..Default::default()
        },
        EmulatedDeviceConfig {
            name: "kvm-ioapic".into(),
            base_gpa: 0xfec0_0000,
            length: 0x1000,
            emu_type: EmulatedDeviceType::X86IoApic,
            ..Default::default()
        },
        EmulatedDeviceConfig {
            name: "kvm-pit".into(),
            emu_type: EmulatedDeviceType::X86Pit,
            ..Default::default()
        },
    ]
}

#[cfg(not(target_arch = "x86_64"))]
fn host_controlled_emu_devices() -> Vec<EmulatedDeviceConfig> {
    Vec::new()
}

#[cfg(target_arch = "x86_64")]
fn host_controlled_passthrough_addresses() -> Vec<PassThroughAddressConfig> {
    alloc::vec![PassThroughAddressConfig {
        base_gpa: 0xfed8_0000,
        length: 0x1000,
    }]
}

#[cfg(not(target_arch = "x86_64"))]
fn host_controlled_passthrough_addresses() -> Vec<PassThroughAddressConfig> {
    Vec::new()
}

/// Represents the list of physical CPUs available for the VM.
#[derive(Debug, Default, Clone)]
pub struct PhysCpuList {
    cpu_num: usize,
    phys_cpu_ids: Option<Vec<usize>>,
    phys_cpu_sets: Option<Vec<usize>>,
}

impl PhysCpuList {
    /// Returns vCpu id list, its pCpu affinity list, and its guest-visible CPU id.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    /// If the guest physical CPU ID is not provided, it will be set to the vCPU ID.
    ///
    /// The guest physical CPU ID is part of the guest-visible topology. It is
    /// intentionally independent from host pCPU affinity so a VM can expose
    /// more vCPUs than the host has physical CPUs.
    ///
    /// Returns a vector of tuples, each tuple contains:
    /// - The vCpu id.
    /// - The pCpu affinity mask, `None` if not set.
    /// - The guest-visible CPU id of the vCpu, equal to vCpu id if not provided.
    pub fn get_vcpu_affinities_pcpu_ids(&self) -> Vec<(usize, Option<usize>, usize)> {
        let mut vcpu_pcpu_tuples = Vec::new();

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

        if let Some(phys_cpu_sets) = &self.phys_cpu_sets {
            for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
                vcpu_pcpu_tuples[vcpu_id].1 = Some(*pcpu_mask_bitmap);
            }
        }

        if let Some(phys_cpu_ids) = &self.phys_cpu_ids {
            for (vcpu_id, phys_id) in phys_cpu_ids.iter().enumerate() {
                vcpu_pcpu_tuples[vcpu_id].2 = *phys_id;
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

    /// Sets the CPU IDs exposed to the guest.
    pub fn set_guest_phys_cpu_ids(&mut self, phys_cpu_ids: Vec<usize>) {
        self.phys_cpu_ids = Some(phys_cpu_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_entry(entry_point: usize) -> AxVMCrateConfig {
        let mut cfg = AxVMCrateConfig::default();
        cfg.kernel.entry_point = entry_point;
        cfg.kernel.kernel_load_addr = 0x20_0000;
        cfg
    }

    #[test]
    fn entry_point_does_not_enable_bios_implicitly() {
        let cfg = config_with_entry(0x8000);

        let vm_config = AxVMConfig::from(cfg);

        assert!(vm_config.image_config.bios_load_gpa.is_none());
    }

    #[test]
    fn explicit_bios_load_addr_enables_bios_gpa() {
        let mut cfg = config_with_entry(0x8000);
        cfg.kernel.enable_bios = true;
        cfg.kernel.bios_load_addr = Some(0x8000);

        let vm_config = AxVMConfig::from(cfg);

        assert_eq!(
            vm_config
                .image_config
                .bios_load_gpa
                .map(|addr| addr.as_usize()),
            Some(0x8000)
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn uefi_boot_does_not_use_builtin_x86_bios_addr() {
        let mut cfg = config_with_entry(0xffff_fff0);
        cfg.kernel.enable_bios = true;
        cfg.kernel.boot_protocol = Some(VMBootProtocol::Uefi);

        let vm_config = AxVMConfig::from(cfg);

        assert!(vm_config.image_config.bios_load_gpa.is_none());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn uefi_boot_uses_explicit_firmware_path_and_load_addr() {
        let mut cfg = config_with_entry(0xffff_fff0);
        cfg.kernel.enable_bios = true;
        cfg.kernel.boot_protocol = Some(VMBootProtocol::Uefi);
        cfg.kernel.uefi_firmware_path = Some(String::from("OVMF_CODE.fd"));
        cfg.kernel.bios_load_addr = Some(0xffc0_0000);

        let vm_config = AxVMConfig::from(cfg);

        assert_eq!(
            vm_config
                .image_config
                .bios_load_gpa
                .map(|addr| addr.as_usize()),
            Some(0xffc0_0000)
        );
    }

    #[test]
    fn uefi_boot_requires_a_firmware_path_or_legacy_fallback() {
        let mut cfg = config_with_entry(0xffff_fff0);
        cfg.kernel.enable_bios = true;
        cfg.kernel.boot_protocol = Some(VMBootProtocol::Uefi);
        cfg.kernel.bios_load_addr = Some(0xffc0_0000);

        assert!(cfg.kernel.validate_boot_config().is_err());

        cfg.kernel.uefi_firmware_path = Some(String::from("OVMF_CODE.fd"));
        assert!(cfg.kernel.validate_boot_config().is_ok());
    }
}
