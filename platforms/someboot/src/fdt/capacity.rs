//! Boot-time CPU capacity parsing using the firmware CPU identity.

use fdt_raw::Fdt;

use super::cpu_nodes_from_fdt;

pub(crate) const CPU_CAPACITY_SCALE: u16 = 1024;

/// A complete DT capacity description for the CPUs selected by the boot path.
/// No CPU clock provider is available here, so frequency factors are equal,
/// as in Linux's early topology initialization when of_clk_get() fails.
pub(crate) struct CpuCapacities<'a> {
    fdt: Fdt<'a>,
    max_raw: u32,
}

impl<'a> CpuCapacities<'a> {
    pub(crate) fn from_fdt(
        fdt: Fdt<'a>,
        hardware_ids: impl Iterator<Item = usize>,
    ) -> Option<Self> {
        let mut capacities = Self { fdt, max_raw: 1 };
        for hardware_id in hardware_ids {
            // Linux discards partial information for the entire possible CPU set.
            capacities.max_raw = capacities.max_raw.max(capacities.raw(hardware_id)?);
        }
        Some(capacities)
    }

    pub(crate) fn get(&self, hardware_id: usize) -> Option<u16> {
        let raw = u64::from(self.raw(hardware_id)?);
        // u32 DMIPS values multiplied by 1024 fit in u64. Preserve Linux's
        // integer truncation, including zero, instead of inventing a floor.
        Some((raw * u64::from(CPU_CAPACITY_SCALE) / u64::from(self.max_raw)) as u16)
    }

    fn raw(&self, hardware_id: usize) -> Option<u32> {
        let mut nodes = cpu_nodes_from_fdt(self.fdt.clone()).filter(|node| {
            node.reg()
                .and_then(|mut regs| regs.next())
                .is_some_and(|reg| reg.address == hardware_id as u64)
        });
        let node = nodes.next()?;
        // Ambiguous identities cannot be assigned a trustworthy capacity.
        if nodes.next().is_some() {
            return None;
        }
        node.find_property("capacity-dmips-mhz")?
            .as_u32_iter()
            .next()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, vec::Vec};

    use fdt_edit::{Fdt as EditFdt, Node, Property};

    use super::*;

    fn fixture(raw: &[Option<u32>]) -> EditFdt {
        let mut fdt = EditFdt::new();
        let root = fdt.root_id();
        let cpus = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(Property::new("#address-cells", 1u32.to_be_bytes().to_vec()));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(Property::new("#size-cells", 0u32.to_be_bytes().to_vec()));
        for (index, capacity) in raw.iter().enumerate() {
            let hardware_id = (index as u32 + 1) * 16;
            let id = fdt.add_node(cpus, Node::new(&format!("cpu@{hardware_id:x}")));
            let node = fdt.node_mut(id).unwrap();
            node.set_property(Property::new("reg", hardware_id.to_be_bytes().to_vec()));
            node.set_property(Property::new("compatible", b"arm,cortex-a55\0".to_vec()));
            if let Some(raw) = capacity {
                node.set_property(Property::new(
                    "capacity-dmips-mhz",
                    raw.to_be_bytes().to_vec(),
                ));
            }
        }
        fdt
    }

    fn capacities(fdt: &EditFdt, ids: &[usize]) -> Vec<u16> {
        let bytes = fdt.encode();
        let fdt = Fdt::from_bytes(bytes.as_ref()).unwrap();
        let capacities = CpuCapacities::from_fdt(fdt, ids.iter().copied());
        ids.iter()
            .map(|&id| {
                capacities
                    .as_ref()
                    .and_then(|c| c.get(id))
                    .unwrap_or(CPU_CAPACITY_SCALE)
            })
            .collect()
    }

    #[test]
    fn capacity_normalization_follows_selected_hardware_ids() {
        let mut fdt = fixture(&[Some(2850), Some(5660), Some(u32::MAX)]);
        // The boot CPU follows the other selected CPU in firmware order. CPUs excluded by the boot
        // selection must not influence the scale of the selected CPU set.
        assert_eq!(capacities(&fdt, &[32, 16]), [1024, 515]);

        // Disabled nodes, nodes without reg, non-CPU children and stray CPU
        // nodes cannot provide capacity or shift the runtime CPU indices.
        let cpus = fdt.get_by_path_id("/cpus").unwrap();
        let disabled = fdt.get_by_path_id("/cpus/cpu@30").unwrap();
        fdt.node_mut(disabled)
            .unwrap()
            .set_property(Property::new("status", b"disabled\0".to_vec()));
        let missing_reg = fdt.add_node(cpus, Node::new("cpu@99"));
        fdt.node_mut(missing_reg)
            .unwrap()
            .set_property(Property::new(
                "capacity-dmips-mhz",
                u32::MAX.to_be_bytes().to_vec(),
            ));
        let non_cpu = fdt.add_node(cpus, Node::new("cpu@88"));
        let node = fdt.node_mut(non_cpu).unwrap();
        node.set_property(Property::new("device_type", b"memory\0".to_vec()));
        node.set_property(Property::new("reg", 32u32.to_be_bytes().to_vec()));
        let stray = fdt.add_node(fdt.root_id(), Node::new("cpu@20"));
        fdt.node_mut(stray)
            .unwrap()
            .set_property(Property::new("reg", 32u32.to_be_bytes().to_vec()));
        assert_eq!(capacities(&fdt, &[32, 16]), [1024, 515]);
        assert_eq!(capacities(&fdt, &[32, 48]), [1024, 1024]);

        assert_eq!(
            capacities(&fixture(&[Some(1), Some(u32::MAX)]), &[16, 32]),
            [0, 1024]
        );
        assert_eq!(capacities(&fixture(&[Some(0), Some(0)]), &[16, 32]), [0, 0]);
    }

    #[test]
    fn incomplete_capacity_information_disables_asymmetry() {
        assert_eq!(
            capacities(&fixture(&[Some(100), None]), &[16, 32]),
            [1024, 1024]
        );
        assert_eq!(
            capacities(&fixture(&[None, Some(100)]), &[16, 32]),
            [1024, 1024]
        );
        assert_eq!(capacities(&fixture(&[None, None]), &[16, 32]), [1024, 1024]);
        assert_eq!(
            capacities(&fixture(&[Some(100), Some(200)]), &[16, 99]),
            [1024, 1024]
        );

        let mut fdt = fixture(&[Some(100), Some(200)]);
        let id = fdt.get_by_path_id("/cpus/cpu@20").unwrap();
        fdt.node_mut(id)
            .unwrap()
            .set_property(Property::new("capacity-dmips-mhz", alloc::vec![1, 2, 3]));
        assert_eq!(capacities(&fdt, &[16, 32]), [1024, 1024]);
    }
}
