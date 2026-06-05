extern crate alloc;

use alloc::format;
#[cfg(pci_dyn_intx_route)]
use alloc::vec::Vec;

#[cfg(pci_dyn_intx_route)]
use fdt_edit::Fdt;
use fdt_edit::{NodeType, PciRange, PciSpace};
use log::{debug, trace, warn};
#[cfg(pci_dyn_intx_route)]
use rdrive::probe::pci::PciAddress;
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        pci::{PciMem32, PciMem64, PcieController, new_driver_generic},
    },
    register::FdtInfo,
};

#[cfg(feature = "rk3588-pcie")]
#[path = "rk3588.rs"]
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

fn probe_generic_ecam(info: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
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
    let Ok(mut intc) = intc.lock() else {
        warn!(
            "failed to lock PCIe legacy IRQ parent device {:?} for phandle {}",
            parent, interrupt.interrupt_parent
        );
        return;
    };

    let irq: usize = intc.setup_irq_by_fdt(&interrupt.specifier).into();
    super::register_legacy_irq_route(0, logical_bus_end, irq);
}

#[cfg(pci_dyn_intx_route)]
pub fn fdt_irq_for_endpoint(
    address: PciAddress,
    interrupt_pin: u8,
) -> Result<Option<usize>, OnProbeError> {
    let Some(result) =
        rdrive::with_fdt(|fdt| resolve_pci_irq_from_fdt(fdt, address, interrupt_pin))
    else {
        return Ok(None);
    };
    result.map(Some)
}

#[cfg(pci_dyn_intx_route)]
fn resolve_pci_irq_from_fdt(
    fdt: &Fdt,
    address: PciAddress,
    interrupt_pin: u8,
) -> Result<usize, OnProbeError> {
    if interrupt_pin == 0 {
        return Err(OnProbeError::other(format!(
            "PCI endpoint {address} has no interrupt pin"
        )));
    }

    let bus = address.bus();
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
            "multiple PCI host nodes in FDT match endpoint {address} with the same bus-range"
        )));
    } else if candidates.len() == 1 {
        candidates[0]
    } else if candidates.is_empty() {
        return Err(OnProbeError::other(format!(
            "no PCI host node in FDT matches endpoint {address}"
        )));
    } else {
        return Err(OnProbeError::other(format!(
            "multiple PCI host nodes in FDT match endpoint {address} without a unique bus-range \
             match"
        )));
    };

    let irq = pci_host
        .child_interrupts(
            address.bus(),
            address.device(),
            address.function(),
            interrupt_pin,
        )
        .map_err(|err| {
            OnProbeError::other(format!(
                "failed to resolve PCI interrupt-map entry for endpoint {address}: {err:?}"
            ))
        })?;

    decode_irq_cells(&irq.irqs).ok_or_else(|| {
        OnProbeError::other(format!(
            "unsupported PCI interrupt specifier {:?} for endpoint {address}",
            irq.irqs
        ))
    })
}

#[cfg(pci_dyn_intx_route)]
fn decode_irq_cells(specifier: &[u32]) -> Option<usize> {
    match specifier {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

#[cfg(feature = "list-pci-devices")]
mod pci_list_devices {
    use log::info;
    use rdrive::probe::pci::{EndpointRc, FnOnProbe};

    use super::*;

    crate::model_register!(
        name: "PCI Device Lister",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[ProbeKind::Pci {
            on_probe: probe as FnOnProbe
        }],
    );

    fn probe(endpoint: &mut EndpointRc, _plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
        info!("PCIe endpoint: {} bars={:?}", &**endpoint, endpoint.bars());
        Err(OnProbeError::NotMatch)
    }
}
