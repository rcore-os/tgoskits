use alloc::format;

use fdt_edit::{PciRange, PciSpace};
use rdrive::{
    PlatformDevice,
    probe::{
        OnProbeError,
        fdt::NodeType,
        pci::{
            EndpointRc, FnOnProbe, PciMem32, PciMem64, PciRange as _, PcieController,
            new_driver_generic,
        },
    },
    register::FdtInfo,
};

crate::register_driver!(
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
    register_fdt_irq(&info, logical_bus_end);

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

pub(super) fn register_fdt_irq(info: &FdtInfo<'_>, logical_bus_end: u8) {
    let Some(interrupt) = info
        .interrupts()
        .into_iter()
        .find(|interrupt| interrupt.name.as_deref() == Some("legacy"))
    else {
        return;
    };
    let Some(_parent) = info.phandle_to_device_id(interrupt.interrupt_parent) else {
        warn!(
            "failed to resolve PCIe legacy IRQ parent phandle {}",
            interrupt.interrupt_parent
        );
        return;
    };

    let Some(irq) = interrupt
        .specifier
        .iter()
        .last()
        .copied()
        .map(|irq| irq as usize)
    else {
        return;
    };
    super::register_legacy_irq_route(0, logical_bus_end, irq);
}

#[cfg(feature = "pci-list-devices")]
mod pci_list_devices {
    use super::*;

    crate::register_driver!(
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
