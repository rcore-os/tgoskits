//! Machine-owned interrupt-controller descriptions for guest device trees.

mod gic;
mod its;
mod phandle;
mod plic;

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};
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
            let Some(profile) = fallback_gic_profile(&machine.emulated_devices) else {
                return Ok(());
            };
            fallback = profile;
            &fallback
        }
    };
    gic::install_registers(tree, profile)
}

fn fallback_gic_profile(devices: &[EmulatedDeviceConfig]) -> Option<GuestGicProfile> {
    let distributor = devices
        .iter()
        .find(|device| device.emu_type == EmulatedDeviceType::InterruptController)?;
    let regions = devices
        .iter()
        .filter(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion)
        .map(|device| GuestMmioRegion {
            base: device.base_gpa,
            length: device.length,
        })
        .collect::<alloc::vec::Vec<_>>();
    if regions.is_empty() {
        return None;
    }
    Some(GuestGicProfile {
        compatible: "arm,gic-v3".into(),
        node_path: alloc::string::String::new(),
        node_phandle: None,
        distributor: GuestMmioRegion {
            base: distributor.base_gpa,
            length: distributor.length,
        },
        cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
            regions,
            stride: AARCH64_GIC_REDISTRIBUTOR_FRAME_SIZE,
        }),
        its: alloc::vec![],
    })
}

#[cfg(test)]
mod tests;
