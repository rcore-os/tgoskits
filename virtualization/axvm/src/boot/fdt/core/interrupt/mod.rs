//! Machine-owned interrupt-controller descriptions for guest device trees.

mod gic;
mod its;
mod phandle;
mod plic;

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) use gic::host_gic_maintenance_intid;
pub(crate) use gic::host_gic_profile;
pub(crate) use plic::host_plic_profile;

use super::tree::FdtTree;
use crate::{
    AxVmResult,
    machine::{GuestGicProfile, GuestPlicProfile},
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

    let fallback = crate::machine::current_machine_profile(cpu_num);
    let Some(profile) = gic_profile.or(fallback.gic.as_ref()) else {
        return Ok(());
    };
    gic::install_registers(tree, profile)
}

#[cfg(test)]
mod tests;
