extern crate alloc;

use alloc::format;
#[cfg(plat_dyn)]
use alloc::vec::Vec;

#[cfg(plat_dyn)]
use fdt_edit::{Fdt, PciInterruptMap};
use fdt_edit::{NodeType, PciRange, PciSpace};
use log::{debug, trace, warn};
#[cfg(plat_dyn)]
use rdrive::probe::pci::{PciInfo, PciIntxRoute};
use rdrive::{
    probe::{
        OnProbeError,
        pci::{PciMem32, PciMem64, PcieController, new_driver_generic},
    },
    register::{FdtInfo, ProbeFdt},
};

#[cfg(plat_dyn)]
use crate::BindingIrq;

#[cfg(feature = "rk3588-pcie")]
mod rk3588;

crate::model_register!(
    name: "Generic PCIe Controller Driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["pci-host-ecam-generic"],
            on_probe: probe_generic_ecam
        }
    ],
);

fn probe_generic_ecam(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let NodeType::Pci(node) = info.node else {
        return Err(OnProbeError::NotMatch);
    };

    let regs = node.regs();
    for reg in &regs {
        trace!(
            "pcie reg: {:#x}, bus: {:#x}",
            reg.address, reg.child_bus_address
        );
    }

    let reg = regs
        .first()
        .ok_or_else(|| OnProbeError::other("PCIe controller has no regs"))?;
    let mmio_base = reg.address as usize;
    let mmio_size = reg.size.unwrap_or(0x1000) as usize;
    let mut drv = new_driver_generic(mmio_base, mmio_size, axklib::mmio::op())
        .map_err(|e| OnProbeError::other(format!("failed to create PCIe controller: {e:?}")))?;

    for range in node.ranges().unwrap_or_default() {
        debug!("pcie range {range:?}");
        set_pcie_mem_range(&mut drv, &range);
    }
    let logical_bus_end = regs
        .iter()
        .map(|reg| reg.child_bus_address as u8)
        .max()
        .unwrap_or(0);
    register_fdt_legacy_irq(&info, logical_bus_end);

    plat_dev.register_pcie(drv);

    Ok(())
}

pub(super) fn set_pcie_mem_range(drv: &mut PcieController, range: &PciRange) {
    match range.space {
        PciSpace::Memory32 => {
            drv.set_mem32(
                PciMem32 {
                    address: range.cpu_address as _,
                    size: range.size as _,
                },
                range.prefetchable,
            );
        }
        PciSpace::Memory64 => {
            drv.set_mem64(
                PciMem64 {
                    address: range.cpu_address,
                    size: range.size,
                },
                range.prefetchable,
            );
        }
        PciSpace::IO => {}
    }
}

pub(super) fn register_fdt_legacy_irq(info: &FdtInfo<'_>, logical_bus_end: u8) {
    let Some(interrupt) = info
        .interrupts()
        .into_iter()
        .find(|interrupt| interrupt.name.as_deref() == Some("legacy"))
    else {
        return;
    };
    let Some(parent) = info.phandle_to_device_id(interrupt.interrupt_parent) else {
        warn!(
            "failed to resolve PCIe legacy IRQ parent phandle {}",
            interrupt.interrupt_parent
        );
        return;
    };

    let Ok(intc) = rdrive::get::<rdif_intc::Intc>(parent) else {
        warn!(
            "failed to get PCIe legacy IRQ parent device {:?} for phandle {}",
            parent, interrupt.interrupt_parent
        );
        return;
    };
    let Ok(intc) = intc.lock() else {
        warn!(
            "failed to lock PCIe legacy IRQ parent device {:?} for phandle {}",
            parent, interrupt.interrupt_parent
        );
        return;
    };

    let irq = intc
        .translate_fdt(&interrupt.specifier)
        .map(|translation| translation.id)
        .ok();
    super::register_native_legacy_irq_route(
        0,
        logical_bus_end,
        BindingIrq::fdt_interrupt_with_controller(parent, interrupt.specifier),
        irq.and_then(axklib::irq::legacy_irq_raw),
    );
}

#[cfg(plat_dyn)]
pub fn fdt_irq_for_endpoint(info: PciInfo) -> Result<Option<BindingIrq>, OnProbeError> {
    let Some(result) = rdrive::with_fdt(|fdt| resolve_pci_irq_from_fdt(fdt, info)) else {
        return Ok(None);
    };
    result.map(Some)
}

#[cfg(plat_dyn)]
fn resolve_pci_irq_from_fdt(fdt: &Fdt, info: PciInfo) -> Result<BindingIrq, OnProbeError> {
    let route = info.intx_route.ok_or_else(|| {
        OnProbeError::other(format!("PCI endpoint {} has no INTx route", info.address))
    })?;

    let bus = info.address.bus();
    let mut candidates = Vec::new();
    let mut exact_range_matches = Vec::new();
    for node in fdt.all_nodes() {
        let NodeType::Pci(pci) = node else {
            continue;
        };

        match pci.bus_range() {
            Some(range) if range.contains(&(bus as u32)) => {
                exact_range_matches.push(pci);
                candidates.push(pci);
            }
            Some(_) => {}
            None => candidates.push(pci),
        }
    }

    let pci_host = if exact_range_matches.len() == 1 {
        exact_range_matches[0]
    } else if exact_range_matches.len() > 1 {
        return Err(OnProbeError::other(format!(
            "multiple PCI host nodes in FDT match endpoint {} with the same bus-range",
            info.address
        )));
    } else if candidates.len() == 1 {
        candidates[0]
    } else if candidates.is_empty() {
        return Err(OnProbeError::other(format!(
            "no PCI host node in FDT matches endpoint {}",
            info.address
        )));
    } else {
        return Err(OnProbeError::other(format!(
            "multiple PCI host nodes in FDT match endpoint {} without a unique bus-range match",
            info.address
        )));
    };

    let bus = pci_host
        .bus_range()
        .map(|range| range.start as u8)
        .unwrap_or(0);
    let Some(interrupt) = pci_interrupt_map_entry(pci_host, bus, route) else {
        return Err(OnProbeError::other(format!(
            "failed to resolve PCI interrupt-map entry for endpoint {}",
            info.address
        )));
    };

    let parent = rdrive::fdt_phandle_to_device_id(interrupt.interrupt_parent).ok_or_else(|| {
        OnProbeError::other(format!(
            "failed to resolve PCI interrupt parent {:?} for endpoint {}",
            interrupt.interrupt_parent, info.address
        ))
    })?;
    Ok(BindingIrq::fdt_interrupt_with_controller(
        parent,
        interrupt.parent_irq,
    ))
}

#[cfg(plat_dyn)]
fn pci_interrupt_map_entry(
    pci_host: fdt_edit::PciNodeView<'_>,
    bus: u8,
    route: PciIntxRoute,
) -> Option<PciInterruptMap> {
    let interrupt_map = pci_host.interrupt_map().ok()?;
    let mask = pci_host.interrupt_map_mask()?;
    let child_addr_cells = 3;
    let child_irq_cells = pci_host.interrupt_cells() as usize;
    let address = encoded_pci_child_address(bus, route.root_device, route.root_function);
    let irq = [u32::from(route.root_pin)];
    let child_address = masked_cells(&address, child_addr_cells, &mask, 0);
    let child_irq = masked_cells(&irq, child_irq_cells, &mask, child_addr_cells);

    interrupt_map
        .into_iter()
        .find(|entry| entry.child_address == child_address && entry.child_irq == child_irq)
}

#[cfg(plat_dyn)]
fn encoded_pci_child_address(bus: u8, device: u8, function: u8) -> [u32; 3] {
    [
        ((u32::from(bus) & 0xff) << 16)
            | ((u32::from(device) & 0x1f) << 11)
            | ((u32::from(function) & 0x07) << 8),
        0,
        0,
    ]
}

#[cfg(plat_dyn)]
fn masked_cells(values: &[u32], len: usize, mask: &[u32], mask_offset: usize) -> Vec<u32> {
    let mut cells = Vec::with_capacity(len);
    for idx in 0..len {
        let value = values.get(idx).copied().unwrap_or(0);
        let mask_value = mask.get(mask_offset + idx).copied().unwrap_or(0xffff_ffff);
        cells.push(value & mask_value);
    }
    cells
}

#[cfg(feature = "list-pci-devices")]
mod pci_list_devices {
    use log::info;
    use rdrive::probe::pci::{FnOnProbe, ProbePci};

    use super::*;

    crate::model_register!(
        name: "PCI Device Lister",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[ProbeKind::Pci {
            on_probe: probe as FnOnProbe
        }],
    );

    fn probe(probe: ProbePci<'_>) -> Result<(), OnProbeError> {
        let endpoint = probe.endpoint();
        info!("PCIe endpoint: {} bars={:?}", endpoint, endpoint.bars());
        Err(OnProbeError::NotMatch)
    }
}
