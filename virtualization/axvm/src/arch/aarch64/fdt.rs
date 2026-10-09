//! AArch64-specific guest device-tree policy.

use std::vec::Vec;

use fdt_edit::Fdt;

use crate::{AxVmResult, boot::fdt::core};

pub(crate) fn host_gic_maintenance_intid(fdt: &Fdt) -> AxVmResult<Option<u32>> {
    core::interrupt::host_gic_maintenance_intid(fdt)
}

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_gic_spi,
        resolve_cpu_index: super::capabilities::resolve_cpu_index,
        host_cpu_count: super::capabilities::host_cpu_count,
        guest_cpu_execution_property: core::tree::is_guest_cpu_execution_property,
    }
}

pub(crate) fn host_fdt_bootarg() -> usize {
    super::capabilities::host_fdt_bootarg()
}

pub(crate) fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    super::capabilities::host_phys_to_virt(paddr)
}

pub(super) fn initrd_start_size_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    Some((ramdisk.load_gpa.as_usize() as u64, ramdisk.size? as u64))
}

pub(super) fn update_cpu_node(
    fdt: &Fdt,
    host_fdt: Option<&Fdt>,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let Some(host) = host_fdt else {
        return Ok(fdt.encode().as_ref().to_vec());
    };
    let bytes = core::cpu::project_cpus(fdt, host, crate_config)?;
    let mut tree = core::tree::FdtTree::from_bytes(&bytes)?;
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| crate::ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;
    // Explicit affinity configurations may expose guest CPU ids that have no
    // host CPU node. Complete those nodes after projecting the host identities.
    tree.ensure_guest_cpu_nodes(host, phys_cpu_ids)?;
    Ok(tree.finish())
}

#[cfg(test)]
mod tests {
    use fdt_edit::{Node, Property};
    use fdt_raw::RegInfo;

    use super::*;

    fn phandle_property(name: &str, value: u32) -> Property {
        let mut property = Property::new(name, Vec::new());
        property.set_u32_ls(&[value]);
        property
    }

    #[test]
    fn provided_guest_dtb_projects_cpu_nodes_without_host_cpu_map() {
        let mut host = Fdt::new();
        let cpus = host.add_node(host.root_id(), Node::new("cpus"));
        for (name, id, phandle) in [("cpu@0", 0, 7), ("cpu@1", 1, 8)] {
            let cpu = host.add_node(cpus, Node::new(name));
            let node = host.node_mut(cpu).unwrap();
            node.set_property(phandle_property("phandle", phandle));
            if id == 0 {
                node.set_property(phandle_property("operating-points-v2", 0x51));
                node.set_property(phandle_property("cpu-idle-states", 0x52));
                node.set_property(phandle_property("cpu-supply", 0x53));
                node.set_property(phandle_property("next-level-cache", 0x54));
            }
            host.view_typed_mut(cpu)
                .unwrap()
                .set_regs(&[RegInfo::new(id, None)]);
        }

        let cpu_map = host.add_node(cpus, Node::new("cpu-map"));
        let cluster = host.add_node(cpu_map, Node::new("cluster0"));
        for (name, phandle) in [("core0", 7), ("core1", 8)] {
            let core = host.add_node(cluster, Node::new(name));
            host.node_mut(core)
                .unwrap()
                .set_property(phandle_property("cpu", phandle));
        }

        let mut config = axvmconfig::GuestConfig::default();
        config.base.phys_cpu_ids = Some(std::vec![0, 2]);
        let guest =
            Fdt::from_bytes(&update_cpu_node(&Fdt::new(), Some(&host), &config).unwrap()).unwrap();

        assert!(guest.get_by_path("/cpus/cpu@0").is_some());
        assert!(guest.get_by_path("/cpus/cpu@2").is_some());
        assert!(guest.get_by_path("/cpus/cpu@1").is_none());
        for property in [
            "operating-points-v2",
            "cpu-idle-states",
            "cpu-supply",
            "next-level-cache",
        ] {
            assert!(
                guest
                    .get_by_path("/cpus/cpu@2")
                    .unwrap()
                    .as_node()
                    .get_property(property)
                    .is_none(),
                "provided-DTB clone inherited host-only property {property}"
            );
        }
        // CPU projection intentionally drops the host-only `cpu-map`; guest
        // startup enumerates the projected CPU nodes by `reg` instead.
        assert!(guest.get_by_path_id("/cpus/cpu-map").is_none());
    }
}
