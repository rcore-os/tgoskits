//! RISC-V-specific guest device-tree policy.

use std::vec::Vec;

use crate::{AxVmResult, boot::fdt::core};

pub(crate) fn guest_fdt_policy() -> core::GuestFdtPolicy {
    core::GuestFdtPolicy {
        patch_runtime: super::capabilities::patch_runtime_fdt,
        patch_provided: super::capabilities::patch_provided_fdt,
        decode_interrupt: super::capabilities::decode_plic_source,
        resolve_cpu_index: super::capabilities::resolve_cpu_index,
        host_cpu_count: super::capabilities::host_cpu_count,
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

pub(super) fn ensure_chosen_from_host(
    guest_dtb: Vec<u8>,
    host_fdt: Option<&fdt_edit::Fdt>,
) -> AxVmResult<Vec<u8>> {
    let Some(host_fdt) = host_fdt else {
        return Ok(guest_dtb);
    };
    let Some(host_chosen) = host_fdt.get_by_path_id("/chosen") else {
        return Ok(guest_dtb);
    };
    let mut guest = core::tree::FdtTree::from_bytes(&guest_dtb)?;
    if let Some(guest_chosen) = guest.inner().get_by_path_id("/chosen") {
        merge_missing_host_subtree(&mut guest, guest_chosen, host_fdt, host_chosen)?;
    } else {
        guest.copy_subtree_from(host_fdt, host_chosen, guest.inner().root_id(), false)?;
    }
    Ok(guest.finish())
}

fn merge_missing_host_subtree(
    guest: &mut core::tree::FdtTree,
    guest_node: fdt_edit::NodeId,
    host: &fdt_edit::Fdt,
    host_node: fdt_edit::NodeId,
) -> AxVmResult {
    let host_node = host
        .node(host_node)
        .ok_or_else(|| crate::ax_err_type!(InvalidData, "host FDT node id is invalid"))?;
    for property in host_node.properties() {
        let guest_has_property = guest
            .inner()
            .node(guest_node)
            .and_then(|node| node.get_property(property.name()))
            .is_some();
        if !guest_has_property {
            guest.set_property(guest_node, property.clone())?;
        }
    }

    for &host_child in host_node.children() {
        let host_child_node = host
            .node(host_child)
            .ok_or_else(|| crate::ax_err_type!(InvalidData, "host FDT child id is invalid"))?;
        let guest_child = guest
            .inner()
            .node(guest_node)
            .and_then(|node| node.get_child(host_child_node.name()));
        if let Some(guest_child) = guest_child {
            merge_missing_host_subtree(guest, guest_child, host, host_child)?;
        } else {
            guest.copy_subtree_from(host, host_child, guest_node, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use fdt_edit::{Fdt, Node, Property};

    use super::*;
    use crate::boot::fdt::core::tree::{FdtTree, prop_string};

    #[test]
    fn host_chosen_properties_coexist_with_guest_cmdline_initrd_and_serial_overrides() {
        let host = fdt_with_chosen(&[
            prop_string("bootargs", "root=/dev/host ro"),
            prop_string("stdout-path", "/host-uart"),
            prop_string("host-preserved", "yes"),
        ]);
        let guest = fdt_with_chosen(&[prop_string("guest-preserved", "yes")]);

        let merged =
            ensure_chosen_from_host(guest.encode().as_ref().to_vec(), Some(&host)).unwrap();
        let mut tree = FdtTree::from_bytes(&merged).unwrap();
        tree.patch_chosen(Some((0x8800_0000, 0x20_0000)), Some("root=/dev/vda rw"))
            .unwrap();
        let chosen = tree.inner().get_by_path_id("/chosen").unwrap();
        tree.set_property(chosen, prop_string("stdout-path", "/guest-uart"))
            .unwrap();

        let bytes = tree.finish();
        let result = Fdt::from_bytes(&bytes).unwrap();
        let chosen = result.get_by_path("/chosen").unwrap();
        let chosen = chosen.as_node();
        assert_eq!(
            chosen.get_property("host-preserved").unwrap().as_str(),
            Some("yes")
        );
        assert_eq!(
            chosen.get_property("guest-preserved").unwrap().as_str(),
            Some("yes")
        );
        assert_eq!(
            chosen.get_property("bootargs").unwrap().as_str(),
            Some("root=/dev/vda rw fsck.repair=yes")
        );
        assert_eq!(
            chosen.get_property("linux,initrd-start").unwrap().get_u64(),
            Some(0x8800_0000)
        );
        assert_eq!(
            chosen.get_property("linux,initrd-end").unwrap().get_u64(),
            Some(0x8820_0000)
        );
        assert_eq!(
            chosen.get_property("stdout-path").unwrap().as_str(),
            Some("/guest-uart")
        );
    }

    #[test]
    fn copies_full_host_chosen_subtree_when_guest_has_no_chosen() {
        let mut host = fdt_with_chosen(&[prop_string("host-preserved", "yes")]);
        let host_chosen = host.get_by_path_id("/chosen").unwrap();
        let host_child = add_child_with_property(
            &mut host,
            host_chosen,
            "host-child",
            "host-child-property",
            "yes",
        );
        add_child_with_property(
            &mut host,
            host_child,
            "nested-child",
            "nested-property",
            "yes",
        );
        let guest = Fdt::new();

        let merged =
            ensure_chosen_from_host(guest.encode().as_ref().to_vec(), Some(&host)).unwrap();
        let result = Fdt::from_bytes(&merged).unwrap();

        assert_eq!(
            result
                .get_by_path("/chosen/host-child")
                .unwrap()
                .as_node()
                .get_property("host-child-property")
                .unwrap()
                .as_str(),
            Some("yes")
        );
        assert_eq!(
            result
                .get_by_path("/chosen/host-child/nested-child")
                .unwrap()
                .as_node()
                .get_property("nested-property")
                .unwrap()
                .as_str(),
            Some("yes")
        );
    }

    #[test]
    fn merges_missing_host_chosen_children_without_overwriting_guest_collisions() {
        let mut host = fdt_with_chosen(&[
            prop_string("host-only", "host"),
            prop_string("shared", "host"),
        ]);
        let host_chosen = host.get_by_path_id("/chosen").unwrap();
        add_child_with_property(&mut host, host_chosen, "host-child", "source", "host");
        let host_shared =
            add_child_with_property(&mut host, host_chosen, "shared-child", "shared", "host");
        add_child_with_property(&mut host, host_shared, "host-nested", "source", "host");

        let mut guest = fdt_with_chosen(&[
            prop_string("guest-only", "guest"),
            prop_string("shared", "guest"),
        ]);
        let guest_chosen = guest.get_by_path_id("/chosen").unwrap();
        add_child_with_property(&mut guest, guest_chosen, "guest-child", "source", "guest");
        let guest_shared =
            add_child_with_property(&mut guest, guest_chosen, "shared-child", "shared", "guest");
        add_child_with_property(&mut guest, guest_shared, "guest-nested", "source", "guest");

        let merged =
            ensure_chosen_from_host(guest.encode().as_ref().to_vec(), Some(&host)).unwrap();
        let result = Fdt::from_bytes(&merged).unwrap();
        let chosen = result.get_by_path("/chosen").unwrap();
        assert_eq!(
            chosen.as_node().get_property("host-only").unwrap().as_str(),
            Some("host")
        );
        assert_eq!(
            chosen
                .as_node()
                .get_property("guest-only")
                .unwrap()
                .as_str(),
            Some("guest")
        );
        assert_eq!(
            chosen.as_node().get_property("shared").unwrap().as_str(),
            Some("guest")
        );
        assert!(result.get_by_path("/chosen/host-child").is_some());
        assert!(result.get_by_path("/chosen/guest-child").is_some());
        let shared = result.get_by_path("/chosen/shared-child").unwrap();
        assert_eq!(
            shared.as_node().get_property("shared").unwrap().as_str(),
            Some("guest")
        );
        assert!(
            result
                .get_by_path("/chosen/shared-child/host-nested")
                .is_some()
        );
        assert!(
            result
                .get_by_path("/chosen/shared-child/guest-nested")
                .is_some()
        );
    }

    fn fdt_with_chosen(properties: &[Property]) -> Fdt {
        let mut fdt = Fdt::new();
        let mut chosen = Node::new("chosen");
        for property in properties {
            chosen.set_property(property.clone());
        }
        fdt.add_node(fdt.root_id(), chosen);
        fdt
    }

    fn add_child_with_property(
        fdt: &mut Fdt,
        parent: fdt_edit::NodeId,
        name: &str,
        property_name: &str,
        property_value: &str,
    ) -> fdt_edit::NodeId {
        let child = fdt.add_node(parent, Node::new(name));
        fdt.node_mut(child)
            .unwrap()
            .set_property(prop_string(property_name, property_value));
        child
    }
}
