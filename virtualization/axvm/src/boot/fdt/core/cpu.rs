//! CPU execution identity projected into an independent AArch64 guest tree.

use std::vec::Vec;

use fdt_edit::Fdt;

use crate::{AxVmResult, ax_err_type};

pub(crate) fn project_cpus(
    fdt: &Fdt,
    host_fdt: &Fdt,
    crate_config: &axvmconfig::GuestConfig,
) -> AxVmResult<Vec<u8>> {
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;
    let mut tree = super::tree::FdtTree::from_fdt(fdt.clone());
    tree.validate_phandles()?;
    tree.inner_mut().remove_by_path("/cpus");

    // Project CPU execution identity only. Host DVFS, supplies, idle states
    // and cache providers are not guest resource assignments.
    let mut source = super::tree::FdtTree::clone_filtered(host_fdt, |id, path, _| {
        path == "/cpus"
            || (path
                .strip_prefix("/cpus/cpu@")
                .is_some_and(|suffix| !suffix.contains('/'))
                && super::create::need_cpu_node(phys_cpu_ids, host_fdt, id, path))
    })?;
    let paths = source.node_paths();
    for (id, path) in paths {
        if path == "/cpus" || path.starts_with("/cpus/cpu@") {
            let node = source.inner_mut().node_mut(id).unwrap();
            let removed = node
                .properties()
                .iter()
                .filter(|property| {
                    !matches!(
                        property.name(),
                        "#address-cells"
                            | "#size-cells"
                            | "device_type"
                            | "compatible"
                            | "reg"
                            | "enable-method"
                            | "phandle"
                            | "linux,phandle"
                            | "capacity-dmips-mhz"
                            | "clock-frequency"
                            | "status"
                    )
                })
                .map(|property| std::string::String::from(property.name()))
                .collect::<Vec<_>>();
            for name in removed {
                node.remove_property(&name);
            }
        }
    }
    let source_cpus = cpu_nodes(source.inner());
    if source_cpus.len() != phys_cpu_ids.len() {
        return Err(ax_err_type!(
            InvalidData,
            "selected CPU identities are missing from host firmware"
        ));
    }
    if let Some(cpus) = source.inner().get_by_path_id("/cpus") {
        for name in ["phandle", "linux,phandle"] {
            source
                .inner_mut()
                .node_mut(cpus)
                .unwrap()
                .remove_property(name);
        }
    }
    let guest_cpus = cpu_nodes(fdt);
    let mut used = super::references::phandle_index(fdt)?
        .into_keys()
        .collect::<std::collections::BTreeSet<_>>();
    for &(id, address) in &source_cpus {
        let existing = guest_cpus
            .iter()
            .find(|(_, guest_address)| *guest_address == address)
            .or_else(|| (source_cpus.len() == 1 && guest_cpus.len() == 1).then(|| &guest_cpus[0]));
        if existing.is_none() && !guest_cpus.is_empty() {
            return Err(ax_err_type!(
                InvalidData,
                std::format!(
                    "cannot bind host CPU {address:#x} to an explicit guest CPU; matching \
                     hardware identity or a unique single-CPU role is required"
                )
            ));
        }
        let existing_handle = existing
            .map(|(id, _)| super::tree::checked_node_phandle(fdt.node(*id).unwrap()))
            .transpose()?
            .flatten();
        let handle = match existing_handle {
            Some(handle) => handle,
            None => {
                let mut handle = 1_u32;
                while used.contains(&handle) {
                    handle = handle
                        .checked_add(1)
                        .ok_or_else(|| ax_err_type!(InvalidData, "CPU phandle space exhausted"))?;
                }
                if handle == u32::MAX {
                    return Err(ax_err_type!(InvalidData, "CPU phandle space exhausted"));
                }
                used.insert(handle);
                handle
            }
        };
        for name in ["phandle", "linux,phandle"] {
            let mut property = fdt_edit::Property::new(name, Vec::new());
            property.set_u32_ls(&[handle]);
            source.set_property(id, property)?;
        }
    }
    // The source root is not imported and must not share a guest-assigned CPU identity.
    let root = source.inner().root_id();
    for name in ["phandle", "linux,phandle"] {
        source
            .inner_mut()
            .node_mut(root)
            .unwrap()
            .remove_property(name);
    }
    if let Some(host_cpus_id) = source.inner().get_by_path_id("/cpus") {
        tree.copy_subtree_from(source.inner(), host_cpus_id, tree.inner().root_id(), true)?;
    }
    super::create::prune_cpu_references(fdt, &mut tree)?;

    Ok(tree.finish())
}

fn cpu_nodes(fdt: &Fdt) -> Vec<(fdt_edit::NodeId, u64)> {
    fdt.iter_node_ids()
        .filter_map(|id| {
            let path = fdt.path_of(id);
            let suffix = path.strip_prefix("/cpus/cpu@")?;
            if suffix.contains('/') {
                return None;
            }
            let address = fdt.view_typed(id)?.regs().first()?.address;
            Some((id, address))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use fdt_edit::Property;
    use fdt_raw::RegInfo;

    use super::{super::tree::FdtTree, *};

    fn property(name: &str, value: u32) -> Property {
        let mut property = Property::new(name, Vec::new());
        property.set_u32_ls(&[value]);
        property
    }

    #[test]
    fn projected_cpu_preserves_guest_identity_without_host_power_dependencies() {
        let host = Fdt::from_bytes(include_bytes!(
            "../../../../../../os/axvisor/configs/board/orangepi-5-plus.dtb"
        ))
        .unwrap();
        let mut guest = FdtTree::new();
        let cpus = guest.ensure_path("/cpus").unwrap();
        guest
            .set_property(cpus, property("#address-cells", 2))
            .unwrap();
        guest
            .set_property(cpus, property("#size-cells", 0))
            .unwrap();
        let cpu = guest.ensure_path("/cpus/cpu@0").unwrap();
        guest.set_property(cpu, property("phandle", 7)).unwrap();
        guest
            .set_property(cpu, property("#cooling-cells", 2))
            .unwrap();
        guest
            .inner_mut()
            .view_typed_mut(cpu)
            .unwrap()
            .set_regs(&[RegInfo::new(0, None)]);
        let consumer = guest.ensure_path("/consumer").unwrap();
        guest.set_property(consumer, property("cpu", 7)).unwrap();
        let unrelated = guest.ensure_path("/unrelated").unwrap();
        guest
            .set_property(unrelated, property("phandle", 1))
            .unwrap();
        let fan = guest.ensure_path("/fan").unwrap();
        guest.set_property(fan, property("phandle", 8)).unwrap();
        guest
            .set_property(fan, property("#cooling-cells", 2))
            .unwrap();
        for (path, cells) in [
            ("/thermal-zones/zone/cooling-maps/cpu", &[7, 0, 10][..]),
            (
                "/thermal-zones/zone/cooling-maps/mixed",
                &[7, 0, 10, 8, 7, 10][..],
            ),
        ] {
            let map = guest.ensure_path(path).unwrap();
            let mut cooling = Property::new("cooling-device", Vec::new());
            cooling.set_u32_ls(cells);
            guest.set_property(map, cooling).unwrap();
        }
        let mut config = axvmconfig::GuestConfig::default();
        config.base.phys_cpu_ids = Some(std::vec![0x400]);
        config.base.cpu_num = 1;
        let bytes = project_cpus(guest.inner(), &host, &config).unwrap();
        let projected = Fdt::from_bytes(&bytes).unwrap();
        let cpu = projected.get_by_phandle(7.into()).unwrap();
        assert_eq!(cpu.path(), "/cpus/cpu@400");
        assert!(cpu.as_node().get_property("cpu-supply").is_none());
        assert!(cpu.as_node().get_property("operating-points-v2").is_none());
        assert_eq!(
            projected.get_by_phandle(1.into()).unwrap().path(),
            "/unrelated"
        );
        assert!(projected.get_by_path_id("/cpus/cpu-map").is_none());
        assert!(
            projected
                .get_by_path_id("/thermal-zones/zone/cooling-maps/cpu")
                .is_none()
        );
        let mixed = projected
            .get_by_path("/thermal-zones/zone/cooling-maps/mixed")
            .unwrap();
        assert_eq!(
            mixed
                .as_node()
                .get_property("cooling-device")
                .unwrap()
                .get_u32_iter()
                .collect::<Vec<_>>(),
            [8, 7, 10]
        );
        // Consumers must still be accepted by resource discovery after CPU projection.
        super::super::device::find_all_passthrough_devices_from_paths(
            &["/".into()],
            &[],
            &projected,
        )
        .unwrap();
    }
}
