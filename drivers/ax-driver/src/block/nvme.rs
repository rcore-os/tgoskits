extern crate alloc;

use alloc::format;

use log::{info, warn};
use nvme_driver::{Config, Nvme, NvmeBlockDriver, NvmeIntxSource};
use pcie::{CommandRegister, DeviceType, Endpoint};
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{
    PciIrqRequirement,
    block::{PlatformDeviceBlock, ProbePciBlock},
    pci::PciIrqLease,
};

pub const DEVICE_NAME: &str = "nvme";
const DEFAULT_PAGE_SIZE: usize = 0x1000;
const MAX_MSIX_VECTORS: u16 = 65;

crate::model_register!(
    name: "NVMe",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    if probe.endpoint().device_type() != DeviceType::NvmeController {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = probe.endpoint().bar_mmio(0) else {
        return Err(OnProbeError::other("NVMe BAR0 MMIO missing"));
    };

    let address = probe.endpoint().address();
    info!(
        "NVMe PCI endpoint {address}: BAR0={:#x}..{:#x}, int_pin={}, int_line={}",
        bar.start,
        bar.end,
        probe.endpoint().interrupt_pin(),
        probe.endpoint().interrupt_line()
    );

    let msix_result = {
        let vector_count = probe
            .endpoint()
            .msix_table_info()
            .map(|table| table.entries.min(MAX_MSIX_VECTORS))
            .map_err(|_| OnProbeError::Unsupported("NVMe MSI-X table is unavailable"))
            .and_then(|count| {
                if count < 2 {
                    Err(OnProbeError::Unsupported(
                        "NVMe MSI-X needs admin and I/O vectors",
                    ))
                } else {
                    Ok(count)
                }
            });
        vector_count.and_then(|vector_count| {
            let info = probe.info();
            let endpoint = probe.endpoint_mut();
            PciIrqLease::allocate(endpoint, info, vector_count)
        })
    };
    match msix_result {
        Ok(msix) => {
            let vector_count = register_msix_block(probe, bar, msix)?;
            info!("NVMe block device registered at {address} with {vector_count} MSI-X vectors");
            return Ok(());
        }
        Err(OnProbeError::Unsupported(reason)) => {
            info!("NVMe PCI endpoint {address} MSI-X unavailable ({reason}); using legacy INTx")
        }
        Err(err) => {
            warn!("NVMe PCI endpoint {address} MSI-X setup failed: {err}; using legacy INTx")
        }
    }

    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd.remove(CommandRegister::INTERRUPT_DISABLE);
        cmd
    });

    let nvme = Nvme::new(
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::intx(DEFAULT_PAGE_SIZE),
    )
    .map_err(|err| OnProbeError::other(format!("failed to initialize NVMe: {err:?}")))?;
    let intx_source = PciNvmeIntxSource {
        endpoint: probe.take_endpoint(),
    };
    let driver = NvmeBlockDriver::from_nvme(nvme).with_intx_source(intx_source);
    let irq = probe.register_block(driver, PciIrqRequirement::Required)?;
    info!("NVMe block device registered at {address} with irq={irq:?}");
    Ok(())
}

fn register_msix_block(
    mut probe: ProbePci<'_>,
    bar: core::ops::Range<usize>,
    irq_lease: PciIrqLease,
) -> Result<usize, OnProbeError> {
    let vectors = irq_lease.vector_indices();
    let vector_count = vectors.len();

    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    let config = Config::msix(DEFAULT_PAGE_SIZE, vectors)
        .map_err(|err| OnProbeError::other(format!("invalid NVMe MSI-X layout: {err:?}")))?;
    let nvme = Nvme::new(
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        config,
    )
    .map_err(|err| OnProbeError::other(format!("failed to initialize NVMe: {err:?}")))?;
    let driver = NvmeBlockDriver::from_nvme(nvme);

    let (_, _, plat_dev) = probe.into_parts();
    let _legacy_irq = plat_dev.register_irq_bound_block(driver, irq_lease);
    Ok(vector_count)
}

struct PciNvmeIntxSource {
    endpoint: Endpoint,
}

impl NvmeIntxSource for PciNvmeIntxSource {
    fn is_asserted(&self) -> bool {
        self.endpoint.status().interrupt_status()
    }
}
