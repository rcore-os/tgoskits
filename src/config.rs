//! The configuration structure for the VM.
//! The `AxVMCrateConfig` is generated from toml file, and then converted to `AxVMConfig` for the VM creation.

use alloc::string::String;
use alloc::vec::Vec;

use axaddrspace::GuestPhysAddr;
use axdevice_base::EmulatedDeviceConfig;
use axerrno::AxResult;

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

/// A part of `AxVMConfig`, which represents guest VM type.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum VMType {
    /// Host VM, used for boot from Linux like Jailhouse do, named "type1.5".
    VMTHostVM = 0,
    /// Guest RTOS, generally a simple guest OS with most of the resource passthrough.
    #[default]
    VMTRTOS = 1,
    /// Guest Linux, generally a full-featured guest OS with complicated device emulation requirements.
    VMTLinux = 2,
}

impl From<usize> for VMType {
    fn from(value: usize) -> Self {
        match value {
            0 => Self::VMTHostVM,
            1 => Self::VMTRTOS,
            2 => Self::VMTLinux,
            _ => {
                warn!("Unknown VmType value: {}, default to VMTRTOS", value);
                Self::default()
            }
        }
    }
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
    phys_cpu_sets: Option<Vec<usize>>,
    cpu_config: AxVCpuConfig,
    image_config: VMImageConfig,
    memory_regions: Vec<VmMemConfig>,
    emu_devices: Vec<EmulatedDeviceConfig>,
}

impl From<AxVMCrateConfig> for AxVMConfig {
    fn from(cfg: AxVMCrateConfig) -> Self {
        Self {
            id: cfg.id,
            name: cfg.name,
            vm_type: VMType::from(cfg.vm_type),
            cpu_num: cfg.cpu_num,
            phys_cpu_sets: cfg.phys_cpu_sets,
            cpu_config: AxVCpuConfig {
                bsp_entry: GuestPhysAddr::from(cfg.entry_point),
                ap_entry: GuestPhysAddr::from(cfg.entry_point),
            },
            image_config: VMImageConfig {
                kernel_load_gpa: GuestPhysAddr::from(cfg.kernel_load_addr),
                bios_load_gpa: cfg.bios_load_addr.map(GuestPhysAddr::from),
                dtb_load_gpa: cfg.dtb_load_addr.map(GuestPhysAddr::from),
                ramdisk_load_gpa: cfg.ramdisk_load_addr.map(GuestPhysAddr::from),
            },
            memory_regions: cfg.memory_regions,
            emu_devices: cfg.emu_devices,
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

    /// Returns vCpu id list and its corresponding pCpu affinity list.
    /// If the pCpu affinity is None, it means the vCpu will be allocated to any available pCpu randomly.
    pub fn get_vcpu_affinities(&self) -> Vec<(usize, Option<usize>)> {
        let mut vcpu_pcpu_pairs = Vec::new();
        for vcpu_id in 0..self.cpu_num {
            vcpu_pcpu_pairs.push((vcpu_id, None));
        }
        if let Some(phys_cpu_sets) = &self.phys_cpu_sets {
            for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
                vcpu_pcpu_pairs[vcpu_id].1 = Some(*pcpu_mask_bitmap);
            }
        }
        vcpu_pcpu_pairs
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
}

/// A part of `AxVMConfig`, which represents a memory region.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct VmMemConfig {
    /// The start address of the memory region in GPA.
    pub gpa: usize,
    /// The size of the memory region.
    pub size: usize,
    /// The mappings flags of the memory region, refers to `MappingFlags` provided by `axaddrspace`.
    pub flags: usize,
}

/// The configuration structure for the guest VM serialized from a toml file provided by user,
/// and then converted to `AxVMConfig` for the VM creation.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct AxVMCrateConfig {
    // Basic Information
    id: usize,
    name: String,
    vm_type: usize,

    // Resources.
    // The number of virtual CPUs.
    cpu_num: usize,
    // The mask of physical CPUs who can run this VM.
    // * If None, vcpu will be scheduled on available physical CPUs randomly.
    // * If set, each vcpu will be scheduled on the specified physical CPUs.
    //      For example, [0x0101, 0x0010] means:
    //          * vCpu0 can be scheduled at pCpu0 and pCpu2;
    //          * vCpu1 will only be scheduled at pCpu1;
    //      It will phrase an error if the number of vCpus is not equal to the length of `phys_cpu_sets` array.
    phys_cpu_sets: Option<Vec<usize>>,

    entry_point: usize,

    /// The file path of the kernel image.
    pub kernel_path: String,
    /// The load address of the kernel image.
    pub kernel_load_addr: usize,
    /// The file path of the BIOS image, `None` if not used.
    pub bios_path: Option<String>,
    /// The load address of the BIOS image, `None` if not used.
    pub bios_load_addr: Option<usize>,
    /// The file path of the device tree blob (DTB), `None` if not used.
    pub dtb_path: Option<String>,
    /// The load address of the device tree blob (DTB), `None` if not used.
    pub dtb_load_addr: Option<usize>,
    /// The file path of the ramdisk image, `None` if not used.
    pub ramdisk_path: Option<String>,
    /// The load address of the ramdisk image, `None` if not used.
    pub ramdisk_load_addr: Option<usize>,

    disk_path: Option<String>,

    /// Memory Information
    memory_regions: Vec<VmMemConfig>,
    /// Emu device Information
    /// Todo: passthrough devices
    emu_devices: Vec<EmulatedDeviceConfig>,
}

impl AxVMCrateConfig {
    /// Deserialize the toml string to `AxVMCrateConfig`.
    pub fn from_toml(raw_cfg_str: &str) -> AxResult<Self> {
        let config: AxVMCrateConfig = toml::from_str(raw_cfg_str).map_err(|err| {
            axerrno::ax_err_type!(
                InvalidInput,
                alloc::format!("toml deserialize get err {err:?}")
            )
        })?;
        Ok(config)
    }
}
