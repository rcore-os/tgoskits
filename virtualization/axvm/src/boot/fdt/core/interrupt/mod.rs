//! Machine-owned interrupt-controller descriptions for guest device trees.

mod gic;
mod its;
mod phandle;
mod plic;

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) use gic::host_gic_maintenance_intid;
pub(crate) use gic::host_gic_profile;
pub(super) use phandle::controller_node;
pub(crate) use plic::host_plic_profile;

use super::tree::FdtTree;
use crate::{
    AxVmResult,
    machine::{GuestGicProfile, GuestPlicProfile},
};

/// Identifies supported machine controllers independently of firmware node names.
pub(super) fn is_machine_interrupt_provider(node: &fdt_edit::Node) -> bool {
    node.compatibles()
        .any(|compatible| compatible == "arm,gic-v3-its")
        || (node.get_property("interrupt-controller").is_some()
            && node.compatibles().any(|compatible| {
                gic::is_supported(compatible)
                    || plic::is_supported(compatible)
                    || matches!(
                        compatible,
                        "loongson,cpu-interrupt-controller"
                            | "loongson,ls2k2000-eiointc"
                            | "loongson,pch-pic-1.0"
                    )
            }))
}

/// Selects the guest-visible GIC before memory reservations and device planning.
pub(super) fn select_guest_gic(
    current: &GuestGicProfile,
    provided_dtb: Option<&[u8]>,
    passthrough: bool,
) -> AxVmResult<GuestGicProfile> {
    let Some(bytes) = provided_dtb.filter(|_| !passthrough) else {
        return Ok(current.clone());
    };
    let fdt = fdt_edit::Fdt::from_bytes(bytes).map_err(|err| {
        crate::ax_err_type!(
            InvalidData,
            std::format!("Invalid explicit guest DTB: {err:?}")
        )
    })?;
    host_gic_profile(&fdt)?
        .ok_or_else(|| crate::ax_err_type!(InvalidData, "Explicit guest DTB has no supported GIC"))
}

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
