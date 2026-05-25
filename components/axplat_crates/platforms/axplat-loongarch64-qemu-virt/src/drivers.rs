#[cfg(not(feature = "paging"))]
use core::ptr::NonNull;

use ax_driver::{PlatformDevice, probe::OnProbeError};
#[cfg(not(feature = "paging"))]
use ax_plat::mem::{pa, phys_to_virt};
#[cfg(not(feature = "paging"))]
use mmio_api::{MapError, MmioAddr, MmioOp, MmioRaw};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;
const PCI_LEGACY_IRQS: &[usize] = &[16, 17, 18, 19];
#[cfg(not(feature = "paging"))]
static DIRECT_MMIO: DirectMmio = DirectMmio;

#[cfg(not(feature = "paging"))]
struct DirectMmio;

#[cfg(not(feature = "paging"))]
impl MmioOp for DirectMmio {
    fn ioremap(&self, addr: MmioAddr, size: usize) -> Result<MmioRaw, MapError> {
        let ptr = NonNull::new(phys_to_virt(pa!(addr.as_usize())).as_mut_ptr())
            .ok_or(MapError::Invalid)?;
        Ok(unsafe { MmioRaw::new(addr, ptr, size) })
    }

    fn iounmap(&self, _mmio: &MmioRaw) {}
}

ax_driver::model_register!(
    name: "Static PCIe ECAM",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe,
    }],
);

fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if !ax_driver::pci::has_static_endpoint_drivers() {
        return Err(OnProbeError::NotMatch);
    }

    let mem32 = ax_driver::pci::pci_mem32_from_ranges(devices::PCI_RANGES);
    let mem64 = ax_driver::pci::pci_mem64_from_ranges(devices::PCI_RANGES);
    ax_driver::pci::register_static_legacy_irq_routes(PCI_LEGACY_IRQS, PCI_ECAM_SIZE);

    #[cfg(feature = "paging")]
    let mmio_op = axklib::mmio::op();
    #[cfg(not(feature = "paging"))]
    let mmio_op = &DIRECT_MMIO;

    ax_driver::pci::register_ecam_controller_with_mmio_op(
        plat_dev,
        devices::PCI_ECAM_BASE,
        PCI_ECAM_SIZE,
        mem32,
        mem64,
        mmio_op,
    )
}
