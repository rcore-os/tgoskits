use alloc::{format, vec::Vec};

#[cfg(any(
    test,
    feature = "ahci-fdt",
    feature = "cv181x-sdhci",
    feature = "jpeg",
    feature = "k230-sdhci",
    feature = "net",
    feature = "pci",
    feature = "phytium-mci",
    feature = "rga",
    feature = "rknpu",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
    feature = "xhci-mmio"
))]
use dma_api::DmaCoherency;
use rdrive::{
    DeviceId,
    probe::{OnProbeError, acpi::AcpiInfo},
    register::FdtInfo,
};

use crate::{BindingInfo, BindingIrq};

#[cfg(any(
    test,
    feature = "ahci-fdt",
    feature = "cv181x-sdhci",
    feature = "jpeg",
    feature = "k230-sdhci",
    feature = "net",
    feature = "pci",
    feature = "phytium-mci",
    feature = "rga",
    feature = "rknpu",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
    feature = "xhci-mmio"
))]
pub(crate) fn dma_coherency_from_fdt(info: &FdtInfo<'_>) -> DmaCoherency {
    let mut node = info.node;
    loop {
        if node.as_node().get_property("dma-coherent").is_some() {
            return DmaCoherency::Coherent;
        }
        if node.as_node().get_property("dma-noncoherent").is_some() {
            return DmaCoherency::NonCoherent;
        }
        let Some(parent) = next_dma_parent(info, node) else {
            return platform_default_dma_coherency();
        };
        node = parent;
    }
}

#[cfg(any(
    test,
    feature = "ahci-fdt",
    feature = "cv181x-sdhci",
    feature = "jpeg",
    feature = "k230-sdhci",
    feature = "net",
    feature = "pci",
    feature = "phytium-mci",
    feature = "rga",
    feature = "rknpu",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
    feature = "xhci-mmio"
))]
fn next_dma_parent<'a>(
    info: &FdtInfo<'a>,
    node: rdrive::probe::fdt::NodeType<'a>,
) -> Option<rdrive::probe::fdt::NodeType<'a>> {
    let dma_parent = (|| {
        let names = node
            .as_node()
            .get_property("interconnect-names")?
            .as_str_iter();
        let cells = node.as_node().get_property("interconnects")?.get_u32_iter();
        let phandle = dma_mem_interconnect_phandle(names, cells, |phandle| {
            info.get_by_phandle(rdrive::probe::fdt::Phandle::from(phandle))?
                .as_node()
                .get_property("#interconnect-cells")?
                .get_u32()
        })?;
        info.get_by_phandle(rdrive::probe::fdt::Phandle::from(phandle))
    })();

    // Linux falls back to the ordinary device-tree parent when parsing the
    // optional dma-mem interconnect path fails.
    dma_parent.or_else(|| node.parent())
}

#[cfg(any(
    test,
    feature = "ahci-fdt",
    feature = "cv181x-sdhci",
    feature = "jpeg",
    feature = "k230-sdhci",
    feature = "net",
    feature = "pci",
    feature = "phytium-mci",
    feature = "rga",
    feature = "rknpu",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
    feature = "xhci-mmio"
))]
fn dma_mem_interconnect_phandle<'a>(
    mut names: impl Iterator<Item = &'a str>,
    mut cells: impl Iterator<Item = u32>,
    mut provider_cells: impl FnMut(u32) -> Option<u32>,
) -> Option<u32> {
    let dma_mem_index = names.position(|name| name == "dma-mem")?;
    for entry_index in 0..=dma_mem_index {
        let phandle = cells.next()?;
        let argument_cells = provider_cells(phandle)?;
        for _ in 0..argument_cells {
            cells.next()?;
        }
        if entry_index == dma_mem_index {
            return Some(phandle);
        }
    }
    None
}

#[cfg(any(
    test,
    feature = "ahci-fdt",
    feature = "cv181x-sdhci",
    feature = "jpeg",
    feature = "k230-sdhci",
    feature = "net",
    feature = "pci",
    feature = "phytium-mci",
    feature = "rga",
    feature = "rknpu",
    feature = "rockchip-dwmmc",
    feature = "rockchip-sdhci",
    feature = "starfive-jh7110-dwmmc",
    feature = "xhci-mmio"
))]
const fn platform_default_dma_coherency() -> DmaCoherency {
    if cfg!(target_arch = "aarch64") {
        DmaCoherency::NonCoherent
    } else {
        DmaCoherency::Coherent
    }
}

#[cfg(feature = "net")]
pub(crate) fn dma_coherency_from_acpi(info: &AcpiInfo<'_>) -> Result<DmaCoherency, OnProbeError> {
    dma_coherency_from_acpi_cca(info.dma_coherent())
}

#[cfg(any(test, feature = "net", feature = "pci"))]
pub(crate) fn dma_coherency_from_acpi_cca(
    firmware_value: Option<bool>,
) -> Result<DmaCoherency, OnProbeError> {
    match firmware_value {
        Some(true) => Ok(DmaCoherency::Coherent),
        Some(false) => Ok(DmaCoherency::NonCoherent),
        None if !cfg!(target_arch = "aarch64") => Ok(DmaCoherency::Coherent),
        None => Err(OnProbeError::other(
            "ACPI device does not declare _CCA on an architecture that requires it",
        )),
    }
}

pub fn binding_info_from_fdt(info: &FdtInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(resolve_fdt_irq(info)?))
}

pub fn binding_irq_from_named_fdt_interrupt(
    node: &rdrive::probe::fdt::NodeType<'_>,
    name: &str,
) -> Result<Option<BindingIrq>, OnProbeError> {
    let interrupts = node.interrupts();
    if interrupts.is_empty() {
        return Ok(None);
    }

    let index = node
        .as_node()
        .get_property("interrupt-names")
        .and_then(|prop| prop.as_str_iter().position(|irq_name| irq_name == name))
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] interrupt-names does not contain {name}",
                node.name()
            ))
        })?;
    let interrupt = interrupts.get(index).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] interrupt-names entry {name} has no matching interrupts cell",
            node.name()
        ))
    })?;
    let controller =
        rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent).ok_or_else(|| {
            OnProbeError::other(format!(
                "[{}] interrupt-parent {} is not registered",
                node.name(),
                interrupt.interrupt_parent
            ))
        })?;

    Ok(Some(binding_irq_from_fdt_interrupt(
        controller,
        interrupt.specifier.clone(),
    )))
}

pub fn binding_info_from_acpi(info: &AcpiInfo<'_>) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(
        info.irq_route().map(BindingIrq::from),
    ))
}

pub fn binding_info_from_acpi_route(
    _path: &str,
    route: Option<rdrive::probe::acpi::AcpiGsiRoute>,
) -> Result<BindingInfo, OnProbeError> {
    Ok(BindingInfo::with_binding_irq(route.map(BindingIrq::from)))
}

fn resolve_fdt_irq(info: &FdtInfo<'_>) -> Result<Option<BindingIrq>, OnProbeError> {
    let Some(interrupt) = info.interrupts().into_iter().next() else {
        return Ok(None);
    };
    let controller = info
        .phandle_to_device_id(interrupt.interrupt_parent)
        .ok_or_else(|| {
            OnProbeError::other(format!(
                "interrupt-parent {} is not registered",
                interrupt.interrupt_parent
            ))
        })?;

    Ok(Some(binding_irq_from_fdt_interrupt(
        controller,
        interrupt.specifier,
    )))
}

fn binding_irq_from_fdt_interrupt(controller: DeviceId, cells: impl Into<Vec<u32>>) -> BindingIrq {
    BindingIrq::fdt_interrupt_with_controller(controller, cells)
}

#[cfg(feature = "pci")]
pub fn binding_info_from_pci(
    info: rdrive::probe::pci::PciInfo,
    requirement: crate::PciIrqRequirement,
) -> Result<BindingInfo, OnProbeError> {
    let irq = crate::pci::resolve_intx_binding(info)?;
    if irq.is_none() && requirement == crate::PciIrqRequirement::Required {
        return Err(OnProbeError::other(format!(
            "failed to resolve IRQ for PCI endpoint {}",
            info.address
        )));
    }
    Ok(BindingInfo::with_binding_irq(irq))
}

#[cfg(test)]
mod dma_tests {
    extern crate std;

    use alloc::vec::Vec;
    use core::ptr::NonNull;
    use std::{string::String, sync::Mutex};

    use dma_api::DmaCoherency;
    use fdt_edit::{Fdt, Node, Property};
    use rdrive::{
        Platform,
        probe::OnProbeError,
        register::{DriverRegister, ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
    };

    static CAPTURED_COHERENCY: Mutex<Vec<(String, DmaCoherency)>> = Mutex::new(Vec::new());

    static DMA_PROBE_KINDS: &[ProbeKind] = &[ProbeKind::Fdt {
        compatibles: &["test,dma-coherency"],
        on_probe: capture_dma_coherency,
    }];

    fn capture_dma_coherency(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
        CAPTURED_COHERENCY.lock().unwrap().push((
            String::from(probe.info().node.name()),
            super::dma_coherency_from_fdt(probe.info()),
        ));
        Ok(())
    }

    #[test]
    fn fdt_dma_coherency_follows_device_dma_mem_and_parent_precedence() {
        let encoded = dma_coherency_fdt().encode();
        let dtb = std::boxed::Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
        rdrive::init(Platform::Fdt {
            addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
        })
        .expect("DMA coherency test FDT should initialize");
        rdrive::register_add(DriverRegister {
            name: "ax-driver DMA coherency FDT test",
            level: ProbeLevel::PostKernel,
            priority: ProbePriority::DEFAULT,
            probe_kinds: DMA_PROBE_KINDS,
        });

        rdrive::probe_all(true).expect("DMA coherency test devices should probe");

        let captured = CAPTURED_COHERENCY.lock().unwrap();
        assert_captured(&captured, "explicit-coherent", DmaCoherency::Coherent);
        assert_captured(&captured, "explicit-noncoherent", DmaCoherency::NonCoherent);
        assert_captured(&captured, "both-properties", DmaCoherency::Coherent);
        assert_captured(&captured, "parent-coherent", DmaCoherency::Coherent);
        assert_captured(&captured, "parent-noncoherent", DmaCoherency::NonCoherent);
        assert_captured(&captured, "dma-mem-over-parent", DmaCoherency::NonCoherent);
        assert_captured(&captured, "device-over-dma-mem", DmaCoherency::Coherent);
        assert_captured(
            &captured,
            "platform-default",
            super::platform_default_dma_coherency(),
        );
    }

    #[test]
    fn platform_fdt_dma_default_matches_linux_supported_architectures() {
        let expected = if cfg!(target_arch = "aarch64") {
            DmaCoherency::NonCoherent
        } else {
            DmaCoherency::Coherent
        };
        assert_eq!(super::platform_default_dma_coherency(), expected);
    }

    #[test]
    fn dma_mem_interconnect_uses_provider_defined_entry_widths() {
        let names = ["cpu-mem", "dma-mem"].into_iter();
        let cells = [1, 0x10, 0x20, 2, 0x30].into_iter();

        let phandle = super::dma_mem_interconnect_phandle(names, cells, |phandle| match phandle {
            1 => Some(2),
            2 => Some(1),
            _ => None,
        });

        assert_eq!(phandle, Some(2));
    }

    #[test]
    fn malformed_dma_mem_interconnect_is_rejected_for_parent_fallback() {
        let names = ["dma-mem"].into_iter();
        let truncated_cells = [2].into_iter();

        assert_eq!(
            super::dma_mem_interconnect_phandle(names, truncated_cells, |_| Some(1)),
            None
        );
    }

    #[test]
    fn explicit_acpi_cca_overrides_architecture_default() {
        assert_eq!(
            super::dma_coherency_from_acpi_cca(Some(true)).unwrap(),
            DmaCoherency::Coherent
        );
        assert_eq!(
            super::dma_coherency_from_acpi_cca(Some(false)).unwrap(),
            DmaCoherency::NonCoherent
        );
    }

    #[test]
    fn missing_acpi_cca_follows_linux_architecture_contract() {
        let result = super::dma_coherency_from_acpi_cca(None);
        if cfg!(target_arch = "aarch64") {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), DmaCoherency::Coherent);
        }
    }

    fn dma_coherency_fdt() -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();

        let explicit_coherent_parent = fdt.add_node(root, Node::new("coherent-override-bus"));
        fdt.node_mut(explicit_coherent_parent)
            .unwrap()
            .set_property(empty_property("dma-noncoherent"));
        let explicit_coherent =
            fdt.add_node(explicit_coherent_parent, dma_device("explicit-coherent"));
        fdt.node_mut(explicit_coherent)
            .unwrap()
            .set_property(empty_property("dma-coherent"));

        let explicit_noncoherent_parent = fdt.add_node(root, Node::new("noncoherent-override-bus"));
        fdt.node_mut(explicit_noncoherent_parent)
            .unwrap()
            .set_property(empty_property("dma-coherent"));
        let explicit_noncoherent = fdt.add_node(
            explicit_noncoherent_parent,
            dma_device("explicit-noncoherent"),
        );
        fdt.node_mut(explicit_noncoherent)
            .unwrap()
            .set_property(empty_property("dma-noncoherent"));

        let both_properties = fdt.add_node(root, dma_device("both-properties"));
        fdt.node_mut(both_properties)
            .unwrap()
            .set_property(empty_property("dma-coherent"));
        fdt.node_mut(both_properties)
            .unwrap()
            .set_property(empty_property("dma-noncoherent"));

        let coherent_parent = fdt.add_node(root, Node::new("coherent-parent-bus"));
        fdt.node_mut(coherent_parent)
            .unwrap()
            .set_property(empty_property("dma-coherent"));
        fdt.add_node(coherent_parent, dma_device("parent-coherent"));

        let noncoherent_parent = fdt.add_node(root, Node::new("noncoherent-parent-bus"));
        fdt.node_mut(noncoherent_parent)
            .unwrap()
            .set_property(empty_property("dma-noncoherent"));
        fdt.add_node(noncoherent_parent, dma_device("parent-noncoherent"));

        let dma_mem = fdt.add_node(root, Node::new("dma-memory-provider"));
        fdt.node_mut(dma_mem)
            .unwrap()
            .set_property(u32_property("phandle", &[1]));
        fdt.node_mut(dma_mem)
            .unwrap()
            .set_property(u32_property("#interconnect-cells", &[1]));
        fdt.node_mut(dma_mem)
            .unwrap()
            .set_property(empty_property("dma-noncoherent"));

        let dma_mem_parent = fdt.add_node(root, Node::new("dma-mem-parent-bus"));
        fdt.node_mut(dma_mem_parent)
            .unwrap()
            .set_property(empty_property("dma-coherent"));
        let dma_mem_over_parent = fdt.add_node(dma_mem_parent, dma_device("dma-mem-over-parent"));
        set_dma_mem_path(&mut fdt, dma_mem_over_parent, 1);

        let device_over_dma_mem = fdt.add_node(dma_mem_parent, dma_device("device-over-dma-mem"));
        fdt.node_mut(device_over_dma_mem)
            .unwrap()
            .set_property(empty_property("dma-coherent"));
        set_dma_mem_path(&mut fdt, device_over_dma_mem, 1);

        fdt.add_node(root, dma_device("platform-default"));
        fdt
    }

    fn dma_device(name: &str) -> Node {
        let mut node = Node::new(name);
        node.set_property(string_list_property("compatible", &["test,dma-coherency"]));
        node
    }

    fn set_dma_mem_path(fdt: &mut Fdt, node: usize, phandle: u32) {
        fdt.node_mut(node)
            .unwrap()
            .set_property(string_list_property("interconnect-names", &["dma-mem"]));
        fdt.node_mut(node)
            .unwrap()
            .set_property(u32_property("interconnects", &[phandle, 0]));
    }

    fn assert_captured(captured: &[(String, DmaCoherency)], name: &str, expected: DmaCoherency) {
        let actual = captured
            .iter()
            .find_map(|(node, coherency)| (node == name).then_some(*coherency));
        assert_eq!(actual, Some(expected), "unexpected coherency for {name}");
    }

    fn empty_property(name: &str) -> Property {
        Property::new(name, Vec::new())
    }

    fn u32_property(name: &str, values: &[u32]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_be_bytes());
        }
        Property::new(name, data)
    }

    fn string_list_property(name: &str, values: &[&str]) -> Property {
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        Property::new(name, data)
    }
}
