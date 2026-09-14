use crate::{ArchTrait, arch::Arch};

#[derive(Clone)]
enum CpuIdIterState {
    Acpi(CpuIdOrder),
    Fdt(CpuIdOrder),
    Default,
    Done,
}

pub(super) fn cpu_id_list() -> CpuIdIter {
    CpuIdIter::new()
}

#[derive(Clone)]
pub(super) struct CpuIdIter {
    state: CpuIdIterState,
}

impl CpuIdIter {
    fn new() -> Self {
        Self::from_sources(
            Arch::cpu_current_hartid(),
            crate::acpi::cpu_id_list(),
            crate::fdt::cpu_id_list,
        )
    }

    fn from_sources<FdtIds: Iterator<Item = usize>>(
        boot_cpu_id: usize,
        acpi_cpu_ids: Option<impl Iterator<Item = usize>>,
        fdt_cpu_ids: impl FnOnce() -> Option<FdtIds>,
    ) -> Self {
        let state = if let Some(cpu_ids) = acpi_cpu_ids
            && let Some(order) = CpuIdOrder::new(cpu_ids, boot_cpu_id)
        {
            CpuIdIterState::Acpi(order)
        } else if let Some(cpu_ids) = fdt_cpu_ids()
            && let Some(order) = CpuIdOrder::new(cpu_ids, boot_cpu_id)
        {
            CpuIdIterState::Fdt(order)
        } else {
            CpuIdIterState::Default
        };
        Self { state }
    }

    /// Capacity firmware must belong to the selected CPU identity namespace.
    /// ACPI capacity discovery is not implemented, so that source keeps the
    /// homogeneous default even when a conflicting FDT is also available.
    pub(super) fn capacity_fdt<'a>(
        &self,
        read_fdt: impl FnOnce() -> Option<fdt_raw::Fdt<'a>>,
    ) -> Option<fdt_raw::Fdt<'a>> {
        if matches!(self.state, CpuIdIterState::Fdt(_)) {
            read_fdt()
        } else {
            None
        }
    }
}

impl Iterator for CpuIdIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let next_cpu_id = match &mut self.state {
            CpuIdIterState::Acpi(order) => {
                crate::acpi::cpu_id_list().and_then(|cpu_ids| order.next(cpu_ids))
            }
            CpuIdIterState::Fdt(order) => {
                crate::fdt::cpu_id_list().and_then(|cpu_ids| order.next(cpu_ids))
            }
            CpuIdIterState::Default => {
                self.state = CpuIdIterState::Done;
                return Some(0);
            }
            CpuIdIterState::Done => return None,
        };
        if next_cpu_id.is_none() {
            self.state = CpuIdIterState::Done;
        }
        next_cpu_id
    }
}

#[derive(Clone)]
enum CpuIdOrder {
    EmitBoot {
        boot_cpu_id: usize,
    },
    WalkFirmware {
        next_index: usize,
        skip_cpu_id: Option<usize>,
    },
}

impl CpuIdOrder {
    fn new(cpu_ids: impl Iterator<Item = usize>, boot_cpu_id: usize) -> Option<Self> {
        let mut cpu_ids = cpu_ids.peekable();

        cpu_ids.peek()?;

        let contains_boot_cpu = cpu_ids.any(|id| id == boot_cpu_id);

        Some(if contains_boot_cpu {
            Self::EmitBoot { boot_cpu_id }
        } else {
            Self::WalkFirmware {
                next_index: 0,
                skip_cpu_id: None,
            }
        })
    }

    fn next(&mut self, cpu_ids: impl Iterator<Item = usize>) -> Option<usize> {
        match self {
            Self::EmitBoot { boot_cpu_id } => {
                let boot_cpu_id = *boot_cpu_id;
                *self = Self::WalkFirmware {
                    next_index: 0,
                    skip_cpu_id: Some(boot_cpu_id),
                };
                Some(boot_cpu_id)
            }
            Self::WalkFirmware {
                next_index,
                skip_cpu_id,
            } => {
                for (firmware_index, cpu_id) in cpu_ids.enumerate().skip(*next_index) {
                    *next_index = firmware_index + 1;
                    if Some(cpu_id) != *skip_cpu_id {
                        return Some(cpu_id);
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{CpuIdIter, CpuIdOrder};

    #[test]
    fn capacity_uses_the_selected_firmware_source() {
        use fdt_edit::{Fdt, Node, Property};

        let mut tree = Fdt::new();
        let cpus = tree.add_node(tree.root_id(), Node::new("cpus"));
        let node = tree.node_mut(cpus).unwrap();
        node.set_property(Property::new("#address-cells", 1u32.to_be_bytes().to_vec()));
        node.set_property(Property::new("#size-cells", 0u32.to_be_bytes().to_vec()));
        for (name, id, raw) in [("cpu@10", 16u32, 100u32), ("cpu@20", 32, 200)] {
            let cpu = tree.add_node(cpus, Node::new(name));
            let node = tree.node_mut(cpu).unwrap();
            node.set_property(Property::new("reg", id.to_be_bytes().to_vec()));
            node.set_property(Property::new(
                "capacity-dmips-mhz",
                raw.to_be_bytes().to_vec(),
            ));
        }
        let bytes = tree.encode();
        let raw = fdt_raw::Fdt::from_bytes(bytes.as_ref()).unwrap();
        let capacities = |acpi_ids: Option<&[usize]>| {
            let ids = CpuIdIter::from_sources(32, acpi_ids.map(|ids| ids.iter().copied()), || {
                Some([16, 32].into_iter())
            });
            let caps = ids
                .capacity_fdt(|| Some(raw.clone()))
                .and_then(|fdt| crate::fdt::CpuCapacities::from_fdt(fdt, [32, 16].into_iter()));
            [32, 16].map(|id| {
                caps.as_ref()
                    .and_then(|caps| caps.get(id))
                    .unwrap_or(crate::fdt::CPU_CAPACITY_SCALE)
            })
        };
        // Identical numeric IDs do not authorize using FDT capacity for ACPI.
        assert_eq!(capacities(Some(&[16, 32])), [1024, 1024]);
        // An absent or empty ACPI list permits the FDT topology and capacity.
        assert_eq!(capacities(None), [1024, 512]);
        assert_eq!(capacities(Some(&[])), [1024, 512]);
    }

    #[test]
    fn nonzero_boot_cpu_becomes_logical_cpu_zero() {
        let firmware_cpu_ids = [0, 1, 2, 0x103];
        let mut order = CpuIdOrder::new(firmware_cpu_ids.into_iter(), 0x103).unwrap();
        let ordered_cpu_ids: Vec<_> =
            core::iter::from_fn(|| order.next(firmware_cpu_ids.into_iter())).collect();

        assert_eq!(ordered_cpu_ids, [0x103, 0, 1, 2]);
    }

    #[test]
    fn independent_traversals_keep_the_same_boot_cpu_order() {
        let firmware_cpu_ids = [0, 1, 2, 0x103];
        let collect_order = || {
            let mut order = CpuIdOrder::new(firmware_cpu_ids.into_iter(), 0x103).unwrap();
            core::iter::from_fn(|| order.next(firmware_cpu_ids.into_iter())).collect::<Vec<_>>()
        };

        assert_eq!(collect_order(), [0x103, 0, 1, 2]);
        assert_eq!(collect_order(), [0x103, 0, 1, 2]);
    }

    #[test]
    fn firmware_order_is_preserved_when_boot_cpu_is_missing() {
        let firmware_cpu_ids = [1, 2, 3, 4];
        let mut order = CpuIdOrder::new(firmware_cpu_ids.into_iter(), 0).unwrap();
        let ordered_cpu_ids: Vec<_> =
            core::iter::from_fn(|| order.next(firmware_cpu_ids.into_iter())).collect();

        assert_eq!(ordered_cpu_ids, firmware_cpu_ids);
    }
}
