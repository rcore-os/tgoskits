use alloc::format;
#[cfg(not(feature = "paging"))]
use core::ptr::NonNull;

use ax_driver::{PlatformDevice, probe::OnProbeError};
#[cfg(not(feature = "paging"))]
use ax_plat::mem::{pa, phys_to_virt};
use mmio_api::MmioRaw;
#[cfg(not(feature = "paging"))]
use mmio_api::{MapError, MmioAddr, MmioOp};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;
const PCI_LEGACY_IRQS: &[usize] = &[32, 33, 34, 35];
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

fn map_mmio_raw(base: usize, size: usize) -> Result<MmioRaw, mmio_api::MapError> {
    #[cfg(feature = "paging")]
    {
        axklib::mmio::ioremap_raw(base.into(), size)
    }
    #[cfg(not(feature = "paging"))]
    {
        DIRECT_MMIO.ioremap(base.into(), size)
    }
}

fn pcie_mmio_op() -> &'static dyn mmio_api::MmioOp {
    #[cfg(feature = "paging")]
    {
        axklib::mmio::op()
    }
    #[cfg(not(feature = "paging"))]
    {
        &DIRECT_MMIO
    }
}

mod pci_ecam {
    use super::*;

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
        ax_driver::pci::register_ecam_controller_with_mmio_op(
            plat_dev,
            devices::PCI_ECAM_BASE,
            PCI_ECAM_SIZE,
            mem32,
            mem64,
            pcie_mmio_op(),
        )
    }
}

fn register_virtio_mmio(plat_dev: PlatformDevice, index: usize) -> Result<(), OnProbeError> {
    if !ax_driver::virtio::has_static_mmio_drivers() {
        return Err(OnProbeError::NotMatch);
    }

    let Some((base, size)) = devices::VIRTIO_MMIO_RANGES.get(index).copied() else {
        return Err(OnProbeError::NotMatch);
    };

    let mmio = map_mmio_raw(base, size).map_err(|err| {
        OnProbeError::other(format!("failed to map virtio-mmio {base:#x}: {err:?}"))
    })?;
    let Some((ty, transport)) = ax_driver::virtio::probe_mmio_device(mmio.as_ptr(), size) else {
        return Err(OnProbeError::NotMatch);
    };

    ax_driver::virtio::register_static_transport(plat_dev, ty, transport)
}

macro_rules! virtio_mmio_driver {
    ($mod_name:ident, $driver_name:literal, $index:expr) => {
        mod $mod_name {
            use super::*;

            ax_driver::model_register!(
                name: $driver_name,
                level: ProbeLevel::PostKernel,
                priority: ProbePriority::DEFAULT,
                probe_kinds: &[ProbeKind::Static {
                    on_probe: probe,
                }],
            );

            fn probe(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
                register_virtio_mmio(plat_dev, $index)
            }
        }
    };
}

virtio_mmio_driver!(virtio_mmio_0, "Static VirtIO MMIO 0", 0);
virtio_mmio_driver!(virtio_mmio_1, "Static VirtIO MMIO 1", 1);
virtio_mmio_driver!(virtio_mmio_2, "Static VirtIO MMIO 2", 2);
virtio_mmio_driver!(virtio_mmio_3, "Static VirtIO MMIO 3", 3);
virtio_mmio_driver!(virtio_mmio_4, "Static VirtIO MMIO 4", 4);
virtio_mmio_driver!(virtio_mmio_5, "Static VirtIO MMIO 5", 5);
virtio_mmio_driver!(virtio_mmio_6, "Static VirtIO MMIO 6", 6);
virtio_mmio_driver!(virtio_mmio_7, "Static VirtIO MMIO 7", 7);
