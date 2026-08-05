//! Machine-owned interrupt-controller descriptions for guest device trees.

mod gic;
mod its;
mod phandle;
mod plic;

use axvm_types::EmulatedDeviceType;
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) use gic::host_gic_maintenance_intid;
pub(crate) use gic::host_gic_profile;
pub(crate) use plic::host_plic_profile;

use super::tree::FdtTree;
use crate::{
    AxVmResult,
    machine::{
        AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE, GuestGicCpuRegion, GuestGicProfile,
        GuestGicRedistributorProfile, GuestMmioRegion, GuestPlicProfile,
    },
};

/// Rewrites the interrupt-controller resources to match the VM-owned controller.
pub(crate) fn install_machine_interrupt_controller(
    tree: &mut FdtTree,
    cpu_num: usize,
    gic_profile: Option<&GuestGicProfile>,
    plic_profile: Option<&GuestPlicProfile>,
) -> AxVmResult {
    if let Some(profile) = plic_profile {
        return plic::install_registers(tree, profile);
    }

    let fallback;
    let profile = match gic_profile {
        Some(profile) => profile,
        None => {
            let machine = crate::machine::current_machine_profile(cpu_num);
            let distributor = machine
                .emulated_devices
                .iter()
                .find(|device| device.emu_type == EmulatedDeviceType::InterruptController);
            let per_cpu = machine
                .emulated_devices
                .iter()
                .find(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion);
            let (Some(distributor), Some(per_cpu)) = (distributor, per_cpu) else {
                return Ok(());
            };
            fallback = GuestGicProfile {
                compatible: "arm,gic-v3".into(),
                node_path: alloc::string::String::new(),
                node_phandle: None,
                distributor: GuestMmioRegion {
                    base: distributor.base_gpa,
                    length: distributor.length,
                },
                cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                    regions: alloc::vec![GuestMmioRegion {
                        base: per_cpu.base_gpa,
                        length: per_cpu.length,
                    }],
                    stride: AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE,
                }),
                its: alloc::vec![],
            };
            &fallback
        }
    };
    gic::install_registers(tree, profile)
}

#[cfg(test)]
mod tests;
