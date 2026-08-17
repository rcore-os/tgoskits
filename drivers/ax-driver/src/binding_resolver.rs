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

#[cfg(test)]
mod dma_tests {
    use dma_api::DmaCoherency;

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
