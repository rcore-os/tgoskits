//! Generic ECAM host encoding from one resolved guest PCI firmware view.

use core::ops::Range;
use std::{format, vec::Vec};

use axdevice::PCI_BUS_ZERO_ECAM_SIZE;
use fdt_edit::{Fdt, Node, Property};

use super::tree::{FdtTree, prop_string};
use crate::{AxVmError, AxVmResult};

const MEMORY32_SPACE: u32 = 0x0200_0000;

/// Passive firmware view derived from one resolved PCI bus.
///
/// The view is constructed from the two graph-resolved host resources and
/// validates the generic ECAM firmware contract before serialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GuestPciHost {
    ecam_base: u64,
    ecam_size: u64,
    memory_base: u64,
    memory_size: u64,
}

impl GuestPciHost {
    /// Captures and validates the firmware-visible host apertures.
    pub(crate) fn new(ecam: (u64, u64), memory: (u64, u64)) -> AxVmResult<Self> {
        let ecam_end = ecam
            .0
            .checked_add(ecam.1)
            .ok_or_else(|| AxVmError::invalid_config("AArch64 PCI ECAM range overflows u64"))?;
        let memory_end = memory.0.checked_add(memory.1).ok_or_else(|| {
            AxVmError::invalid_config("AArch64 PCI memory aperture overflows u64")
        })?;
        if ecam.1 != PCI_BUS_ZERO_ECAM_SIZE || ecam.0 & (PCI_BUS_ZERO_ECAM_SIZE - 1) != 0 {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI ECAM must be a 1 MiB-aligned 1 MiB window",
            ));
        }
        if memory.1 == 0 || memory_end > (1u64 << 32) {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI memory aperture must be non-empty and below 4 GiB",
            ));
        }
        if ecam.0 < memory_end && memory.0 < ecam_end {
            return Err(AxVmError::invalid_config(
                "AArch64 PCI ECAM and memory aperture overlap",
            ));
        }
        Ok(Self {
            ecam_base: ecam.0,
            ecam_size: ecam.1,
            memory_base: memory.0,
            memory_size: memory.1,
        })
    }

    pub(crate) const fn ecam_base(self) -> u64 {
        self.ecam_base
    }

    pub(crate) const fn ecam_size(self) -> u64 {
        self.ecam_size
    }

    pub(crate) const fn memory_base(self) -> u64 {
        self.memory_base
    }

    pub(crate) const fn memory_size(self) -> u64 {
        self.memory_size
    }

    fn ecam_range(self) -> Range<u64> {
        self.ecam_base..self.ecam_base + self.ecam_size
    }

    fn memory_range(self) -> Range<u64> {
        self.memory_base..self.memory_base + self.memory_size
    }
}

pub(crate) fn install_pci_host(
    fdt_bytes: &[u8],
    host: Option<&GuestPciHost>,
) -> AxVmResult<Vec<u8>> {
    let Some(host) = host.copied() else {
        return Ok(fdt_bytes.to_vec());
    };
    let mut tree = FdtTree::from_bytes(fdt_bytes)?;
    validate_tree(&tree, host)?;
    add_host_node(&mut tree, host)?;
    let bytes = tree.finish();
    Fdt::from_bytes(&bytes).map_err(|error| {
        AxVmError::invalid_config(format!("invalid FDT after adding PCI host: {error:?}"))
    })?;
    Ok(bytes)
}

fn validate_tree(tree: &FdtTree, host: GuestPciHost) -> AxVmResult {
    for node_id in tree.inner().iter_node_ids() {
        let node = tree.inner().node(node_id).ok_or_else(|| {
            AxVmError::invalid_config("FDT node disappeared during PCI validation")
        })?;
        if is_pci_bridge(node) {
            return Err(AxVmError::invalid_config(format!(
                "guest FDT already contains PCI bridge {}",
                tree.inner().path_of(node_id)
            )));
        }
        validate_node_registers(tree, node_id, host)?;
    }

    let root = tree.inner().root_id();
    let root_node = tree
        .inner()
        .node(root)
        .ok_or_else(|| AxVmError::invalid_config("guest FDT has no root node"))?;
    validate_cell_count(root_node.address_cells().unwrap_or(2), "#address-cells")?;
    validate_cell_count(root_node.size_cells().unwrap_or(1), "#size-cells")?;
    let node_path = format!("/pci@{:x}", host.ecam_base());
    if tree.inner().get_by_path_id(&node_path).is_some() {
        return Err(AxVmError::invalid_config(format!(
            "guest FDT path {node_path} already exists"
        )));
    }

    Ok(())
}

/// Recognizes PCI host bridges through the standardized `device_type = "pci"`
/// binding and through the generic CAM/ECAM compatible strings. Linux binds
/// `pci-host-generic` by compatible alone, so a node that omits `device_type`
/// (non-conforming per the DT spec) is still probeable by the guest and must
/// be treated as an existing bridge.
fn is_pci_bridge(node: &Node) -> bool {
    node.is_pci()
        || node.compatibles().any(|compatible| {
            matches!(compatible, "pci-host-ecam-generic" | "pci-host-cam-generic")
        })
}

/// Rejects register windows of any tree node that overlap the injected PCI
/// host apertures. The check covers the whole tree because only excluded or
/// passthrough device ranges reach the resource pools; every other guest DTB
/// device relies on this validation as its sole aperture-conflict guard.
fn validate_node_registers(tree: &FdtTree, node_id: usize, host: GuestPciHost) -> AxVmResult {
    let path = tree.inner().path_of(node_id);
    let Some(view) = tree.inner().view_typed(node_id) else {
        return Ok(());
    };
    for register in view.regs() {
        let Some(size) = register.size.filter(|size| *size != 0) else {
            continue;
        };
        let end = register.address.checked_add(size).ok_or_else(|| {
            AxVmError::invalid_config(format!("FDT register range for {path} overflows"))
        })?;
        let register_range = register.address..end;
        if ranges_overlap(&register_range, &host.ecam_range())
            || ranges_overlap(&register_range, &host.memory_range())
        {
            return Err(AxVmError::invalid_config(format!(
                "FDT register range {register_range:#x?} for {path} conflicts with PCI host"
            )));
        }
    }
    Ok(())
}

fn add_host_node(tree: &mut FdtTree, host: GuestPciHost) -> AxVmResult {
    let root = tree.inner().root_id();
    let root_node = tree
        .inner()
        .node(root)
        .ok_or_else(|| AxVmError::invalid_config("guest FDT has no root node"))?;
    let parent_address_cells = root_node.address_cells().unwrap_or(2);
    let parent_size_cells = root_node.size_cells().unwrap_or(1);
    let node_id = tree.add_node(root, Node::new(&format!("pci@{:x}", host.ecam_base())));
    tree.set_property(node_id, prop_string("compatible", "pci-host-ecam-generic"))?;
    tree.set_property(node_id, prop_string("device_type", "pci"))?;
    tree.set_property(node_id, cell_property("#address-cells", &[3]))?;
    tree.set_property(node_id, cell_property("#size-cells", &[2]))?;
    tree.set_property(node_id, cell_property("#interrupt-cells", &[1]))?;
    tree.set_property(node_id, cell_property("linux,pci-domain", &[0]))?;
    tree.set_property(node_id, cell_property("bus-range", &[0, 0]))?;
    tree.set_property(
        node_id,
        cell_property(
            "reg",
            &[
                encode_cells(host.ecam_base(), parent_address_cells)?,
                encode_cells(host.ecam_size(), parent_size_cells)?,
            ]
            .concat(),
        ),
    )?;

    let mut ranges = vec![MEMORY32_SPACE, 0, host.memory_base() as u32];
    ranges.extend(encode_cells(host.memory_base(), parent_address_cells)?);
    ranges.extend(encode_cells(host.memory_size(), 2)?);
    tree.set_property(node_id, cell_property("ranges", &ranges))?;
    tree.set_property(node_id, Property::new("dma-coherent", Vec::new()))?;
    Ok(())
}

fn cell_property(name: &str, cells: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(cells);
    property
}

fn encode_cells(value: u64, count: u32) -> AxVmResult<Vec<u32>> {
    match count {
        1 => Ok(vec![u32::try_from(value).map_err(|_| {
            AxVmError::invalid_config(format!("FDT value {value:#x} does not fit one cell"))
        })?]),
        2 => Ok(vec![(value >> 32) as u32, value as u32]),
        _ => Err(AxVmError::invalid_config(format!(
            "unsupported FDT cell count {count}"
        ))),
    }
}

fn validate_cell_count(count: u32, property: &'static str) -> AxVmResult {
    if matches!(count, 1 | 2) {
        Ok(())
    } else {
        Err(AxVmError::invalid_config(format!(
            "unsupported root {property} value {count} for PCI host"
        )))
    }
}

fn ranges_overlap(left: &Range<u64>, right: &Range<u64>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use fdt_edit::{Fdt, Node, NodeType, PciRange, PciSpace, Property};
    use fdt_raw::RegInfo;

    use super::{GuestPciHost, install_pci_host};

    fn base_fdt() -> Vec<u8> {
        fdt_with_root_cells(2, 2)
    }

    fn fdt_with_root_cells(address_cells: u32, size_cells: u32) -> Vec<u8> {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let mut address_property = Property::new("#address-cells", Vec::new());
        address_property.set_u32_ls(&[address_cells]);
        fdt.node_mut(root).unwrap().set_property(address_property);
        let mut size_property = Property::new("#size-cells", Vec::new());
        size_property.set_u32_ls(&[size_cells]);
        fdt.node_mut(root).unwrap().set_property(size_property);
        fdt.encode().as_ref().to_vec()
    }

    fn host() -> GuestPciHost {
        GuestPciHost::new((0x0b00_0000, 0x10_0000), (0x0c00_0000, 0x0400_0000)).unwrap()
    }

    #[test]
    fn resolved_host_encodes_generic_ecam_and_one_memory32_range() {
        let bytes = install_pci_host(&base_fdt(), Some(&host())).unwrap();
        let fdt = Fdt::from_bytes(&bytes).unwrap();
        let node = fdt
            .find_compatible(&["pci-host-ecam-generic"])
            .into_iter()
            .next()
            .unwrap();
        let NodeType::Pci(pci) = node else {
            panic!("generic ECAM node must parse as a PCI bridge");
        };

        assert_eq!(pci.bus_range(), Some(0..0));
        assert_eq!(pci.regs()[0].address, 0x0b00_0000);
        assert_eq!(pci.regs()[0].size, Some(0x10_0000));
        assert_eq!(
            pci.ranges().unwrap(),
            [PciRange {
                space: PciSpace::Memory32,
                bus_address: 0x0c00_0000,
                cpu_address: 0x0c00_0000,
                size: 0x0400_0000,
                prefetchable: false,
            }]
        );
        let node = fdt.node(pci.id()).unwrap();
        assert_eq!(
            node.get_property("linux,pci-domain").unwrap().get_u32(),
            Some(0)
        );
        assert!(node.get_property("dma-coherent").is_some());
        for absent in [
            "interrupt-map",
            "interrupt-map-mask",
            "msi-map",
            "msi-map-mask",
            "msi-parent",
        ] {
            assert!(node.get_property(absent).is_none(), "unexpected {absent}");
        }
    }

    #[test]
    fn absent_resolved_host_keeps_the_fdt_without_a_pci_node() {
        let original = base_fdt();
        let bytes = install_pci_host(&original, None).unwrap();
        assert_eq!(bytes, original);
        let fdt = Fdt::from_bytes(&bytes).unwrap();
        assert!(fdt.find_compatible(&["pci-host-ecam-generic"]).is_empty());
    }

    #[test]
    fn existing_pci_bridge_is_rejected_without_replacement() {
        let mut fdt = Fdt::from_bytes(&base_fdt()).unwrap();
        let root = fdt.root_id();
        let pci = fdt.add_node(root, Node::new("pcie@20000000"));
        let mut device_type = Property::new("device_type", Vec::new());
        device_type.set_string("pci");
        fdt.node_mut(pci).unwrap().set_property(device_type);
        let bytes = fdt.encode().as_ref().to_vec();

        let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
        assert!(error.to_string().contains("/pcie@20000000"));
    }

    #[test]
    fn conflicting_top_level_register_range_is_rejected() {
        for (name, base) in [
            ("ecam-user@b000000", 0x0b00_0000),
            ("memory-user@c000000", 0x0c00_0000),
        ] {
            let mut fdt = Fdt::from_bytes(&base_fdt()).unwrap();
            let root = fdt.root_id();
            let device = fdt.add_node(root, Node::new(name));
            fdt.view_typed_mut(device)
                .unwrap()
                .set_regs(&[RegInfo::new(base, Some(0x1000))]);
            let bytes = fdt.encode().as_ref().to_vec();

            let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
            assert!(error.to_string().contains(name));
            assert!(error.to_string().contains("conflicts with PCI host"));
        }
    }

    #[test]
    fn overflowing_top_level_register_range_is_rejected() {
        let mut fdt = Fdt::from_bytes(&base_fdt()).unwrap();
        let root = fdt.root_id();
        let device = fdt.add_node(root, Node::new("broken@ffffffffffffff00"));
        fdt.view_typed_mut(device)
            .unwrap()
            .set_regs(&[RegInfo::new(u64::MAX - 0xff, Some(0x200))]);
        let bytes = fdt.encode().as_ref().to_vec();

        let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
        assert!(error.to_string().contains("overflows"));
    }

    #[test]
    fn existing_generic_compatible_bridge_is_rejected_without_device_type() {
        let mut fdt = Fdt::from_bytes(&base_fdt()).unwrap();
        let root = fdt.root_id();
        let pci = fdt.add_node(root, Node::new("pcie@20000000"));
        let mut compatible = Property::new("compatible", Vec::new());
        compatible.set_string("pci-host-ecam-generic");
        fdt.node_mut(pci).unwrap().set_property(compatible);
        let bytes = fdt.encode().as_ref().to_vec();

        let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
        assert!(error.to_string().contains("/pcie@20000000"));
    }

    #[test]
    fn nested_register_range_conflict_is_rejected() {
        let mut fdt = Fdt::from_bytes(&base_fdt()).unwrap();
        let root = fdt.root_id();
        let soc = fdt.add_node(root, Node::new("soc"));
        let uart = fdt.add_node(soc, Node::new("uart@c100000"));
        fdt.view_typed_mut(uart)
            .unwrap()
            .set_regs(&[RegInfo::new(0x0c10_0000, Some(0x1000))]);
        let bytes = fdt.encode().as_ref().to_vec();

        let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
        assert!(error.to_string().contains("/soc/uart@c100000"));
        assert!(error.to_string().contains("conflicts with PCI host"));
    }

    #[test]
    fn unsupported_root_cell_geometry_is_rejected() {
        let bytes = fdt_with_root_cells(3, 2);
        let error = install_pci_host(&bytes, Some(&host())).unwrap_err();
        assert!(error.to_string().contains("#address-cells"));
    }
}
