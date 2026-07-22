//! Guest boot image address planning.

pub(crate) use crate::config::GuestBootPolicy;
use crate::{
    GuestPhysAddr,
    config::{AxVMConfig, VMBootProtocol},
};

const BIOS_RESERVED_SIZE: usize = 2 * 1024 * 1024;

/// Boot image placement facts derived from prepared VM memory.
pub(crate) struct BootImagePlan {
    main_memory_gpa: GuestPhysAddr,
    main_memory_identical: bool,
}

impl BootImagePlan {
    pub(crate) const fn new(main_memory_gpa: GuestPhysAddr, main_memory_identical: bool) -> Self {
        Self {
            main_memory_gpa,
            main_memory_identical,
        }
    }

    pub(crate) fn apply_to_config(&self, config: &mut AxVMConfig) {
        let Some(kernel_load_gpa) = self.adjusted_kernel_load_gpa(config) else {
            return;
        };
        config.relocate_kernel_image(kernel_load_gpa);
    }

    fn adjusted_kernel_load_gpa(&self, config: &AxVMConfig) -> Option<GuestPhysAddr> {
        let GuestBootPolicy::AdjustKernelForBootProtocol { protocol } = config.boot_policy() else {
            return None;
        };
        if protocol == VMBootProtocol::Uefi || !self.main_memory_identical {
            return None;
        }

        let mut kernel_addr = self.main_memory_gpa;
        if protocol == VMBootProtocol::Multiboot && config.image_config().bios_load_gpa.is_some() {
            kernel_addr += BIOS_RESERVED_SIZE;
        }
        Some(kernel_addr)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::*;
    use crate::config::{AxVCpuConfig, AxVMConfig, AxVMConfigParams, PhysCpuList, VMImageConfig};

    fn config_for_boot_policy(
        protocol: VMBootProtocol,
        bios_load_gpa: Option<usize>,
    ) -> AxVMConfig {
        AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: String::from("boot-policy-test"),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            cpu_config: AxVCpuConfig {
                bsp_entry: GuestPhysAddr::from(0x101000),
                ap_entry: GuestPhysAddr::from(0x102000),
                ..Default::default()
            },
            image_config: VMImageConfig {
                kernel_load_gpa: GuestPhysAddr::from(0x100000),
                bios_load_gpa: bios_load_gpa.map(GuestPhysAddr::from),
                ..Default::default()
            },
            boot_policy: GuestBootPolicy::AdjustKernelForBootProtocol { protocol },
            ..Default::default()
        })
    }

    #[test]
    fn boot_policy_moves_multiboot_kernel_after_reserved_bios_space() {
        let mut config = config_for_boot_policy(VMBootProtocol::Multiboot, Some(0x8000));
        let plan = BootImagePlan::new(GuestPhysAddr::from(0x4000_0000), true);

        plan.apply_to_config(&mut config);

        assert_eq!(
            config.image_config().kernel_load_gpa.as_usize(),
            0x4020_0000
        );
        assert_eq!(config.bsp_entry().as_usize(), 0x4020_1000);
        assert_eq!(config.ap_entry().as_usize(), 0x4020_2000);
    }

    #[test]
    fn boot_policy_keeps_uefi_kernel_load_address() {
        let mut config = config_for_boot_policy(VMBootProtocol::Uefi, Some(0x2000_0000));
        let plan = BootImagePlan::new(GuestPhysAddr::from(0x4000_0000), true);

        plan.apply_to_config(&mut config);

        assert_eq!(config.image_config().kernel_load_gpa.as_usize(), 0x100000);
        assert_eq!(config.bsp_entry().as_usize(), 0x101000);
        assert_eq!(config.ap_entry().as_usize(), 0x102000);
    }

    #[test]
    fn keep_configured_boot_policy_does_not_relocate_identical_memory() {
        let mut config = AxVMConfig::new(AxVMConfigParams {
            id: 1,
            name: String::from("boot-policy-test"),
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            cpu_config: AxVCpuConfig {
                bsp_entry: GuestPhysAddr::from(0x101000),
                ap_entry: GuestPhysAddr::from(0x102000),
            },
            image_config: VMImageConfig {
                kernel_load_gpa: GuestPhysAddr::from(0x100000),
                bios_load_gpa: Some(GuestPhysAddr::from(0x8000)),
                ..Default::default()
            },
            boot_policy: GuestBootPolicy::KeepConfigured,
            ..Default::default()
        });
        let plan = BootImagePlan::new(GuestPhysAddr::from(0x4000_0000), true);

        plan.apply_to_config(&mut config);

        assert_eq!(config.image_config().kernel_load_gpa.as_usize(), 0x100000);
        assert_eq!(config.bsp_entry().as_usize(), 0x101000);
        assert_eq!(config.ap_entry().as_usize(), 0x102000);
    }
}
