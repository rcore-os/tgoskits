extern crate alloc;

use alloc::format;

use fdt_edit::{PciRange, PciSpace};
use heapless::Vec as ArrayVec;
use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, fdt::NodeType, pci::*},
    register::FdtInfo,
};
use spin::Mutex;

mod rk3588;

const MAX_PCIE_LEGACY_IRQS: usize = 8;

module_driver!(
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

#[derive(Clone, Copy)]
struct LegacyIrqRoute {
    bus_start: u8,
    bus_end: u8,
    irq: usize,
}

static LEGACY_IRQ_ROUTES: Mutex<ArrayVec<LegacyIrqRoute, MAX_PCIE_LEGACY_IRQS>> =
    Mutex::new(ArrayVec::new());

pub(crate) fn legacy_irq_for_address(address: PciAddress) -> Option<usize> {
    let bus = address.bus();
    LEGACY_IRQ_ROUTES
        .lock()
        .iter()
        .find(|route| bus >= route.bus_start && bus <= route.bus_end)
        .map(|route| route.irq)
}

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
    let mut drv = new_driver_generic(mmio_base, mmio_size, &crate::boot::Kernel)
        .map_err(|e| OnProbeError::other(format!("failed to create PCIe controller: {e:?}")))?;

    for range in node.ranges().unwrap_or_default() {
        debug!("pcie range {range:?}");
        set_pcie_mem_range(&mut drv, &range);
    }

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

pub(super) fn register_legacy_irq(info: &FdtInfo<'_>, logical_bus_end: u8) {
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

    let irq = somehal::irq::irq_setup_by_fdt(parent, &interrupt.specifier).raw();
    let mut routes = LEGACY_IRQ_ROUTES.lock();
    if routes
        .iter()
        .any(|route| route.bus_start == 0 && route.bus_end == logical_bus_end && route.irq == irq)
    {
        return;
    }
    if routes
        .push(LegacyIrqRoute {
            bus_start: 0,
            bus_end: logical_bus_end,
            irq,
        })
        .is_err()
    {
        warn!("too many PCIe legacy IRQ routes; dropping IRQ {}", irq);
    } else {
        info!(
            "PCIe legacy IRQ route: logical bus 0..={} -> IRQ {}",
            logical_bus_end, irq
        );
    }
}

#[cfg(feature = "pci-list-devices")]
mod pci_list_devices {
    use super::*;

    module_driver!(
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
