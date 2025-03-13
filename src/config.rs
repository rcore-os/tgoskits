//! The configuration structure for the VM.
//! The `AxVMCrateConfig` is generated from toml file, and then converted to `AxVMConfig` for the VM creation.

use alloc::string::String;
use alloc::vec::Vec;

use axaddrspace::GuestPhysAddr;

pub use axvmconfig::{
    AxVMCrateConfig, EmulatedDeviceConfig, PassThroughDeviceConfig, VMType, VmMemConfig,
    VmMemMappingType,
};

/// A part of `AxVCpuConfig`, which represents an architecture-dependent `VCpu`.
///
/// The concrete type of configuration is defined in `AxArchVCpuImpl`.
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
#[derive(Debug, Default)]
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
    cpu_num: usize,
    phys_cpu_ids: Option<Vec<usize>>,
    phys_cpu_sets: Option<Vec<usize>>,
    cpu_config: AxVCpuConfig,
    image_config: VMImageConfig,
    memory_regions: Vec<VmMemConfig>,
    emu_devices: Vec<EmulatedDeviceConfig>,
    pass_through_devices: Vec<PassThroughDeviceConfig>,
}

impl From<AxVMCrateConfig> for AxVMConfig {
    fn from(cfg: AxVMCrateConfig) -> Self {
        Self {
            id: cfg.base.id,
            name: cfg.base.name,
            vm_type: VMType::from(cfg.base.vm_type),
            cpu_num: cfg.base.cpu_num,
            phys_cpu_ids: cfg.base.phys_cpu_ids,
            phys_cpu_sets: cfg.base.phys_cpu_sets,
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
            memory_regions: cfg.kernel.memory_regions,
            emu_devices: cfg.devices.emu_devices,
            pass_through_devices: cfg.devices.passthrough_devices,
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

    /// Returns configurations related to VM memory regions.
    pub fn memory_regions(&self) -> &Vec<VmMemConfig> {
        &self.memory_regions
    }

    /// Returns configurations related to VM emulated devices.
    pub fn emu_devices(&self) -> &Vec<EmulatedDeviceConfig> {
        &self.emu_devices
    }

    /// Returns configurations related to VM passthrough devices.
    pub fn pass_through_devices(&self) -> &Vec<PassThroughDeviceConfig> {
        &self.pass_through_devices
    }
}
