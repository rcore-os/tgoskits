extern crate alloc;

use alloc::format;

use log::info;
use nvme_driver::{Config, NvmeBlockDriver};
use pcie::{CommandRegister, DeviceType};
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{
    PciIrqRequirement, binding_info_from_pci_endpoint,
    block::PlatformDeviceBlock,
    pci::{PciIntxIrqLease, PciIrqLease},
};

pub const DEVICE_NAME: &str = "nvme";
const DEFAULT_PAGE_SIZE: usize = 0x1000;
const DEFAULT_IO_QUEUE_PAIRS: usize = 1;

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
        let info = probe.info();
        let endpoint = probe.endpoint_mut();
        PciIrqLease::allocate(endpoint, info, DEFAULT_IO_QUEUE_PAIRS as u16)
    };
    match msix_result {
        Ok(msix) => {
            let irq = register_msix_block(probe, bar, msix)?;
            info!("NVMe block device registered at {address} with MSI-X irqs={irq:?}");
            return Ok(());
        }
        Err(OnProbeError::Unsupported(reason)) => {
            info!("NVMe PCI endpoint {address} MSI-X unavailable ({reason}); using legacy INTx")
        }
        Err(err) => return Err(err),
    }

    let binding = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    PciIntxIrqLease::mask_for_discovery(probe.endpoint_mut());
    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    let driver = NvmeBlockDriver::discover(
        DEVICE_NAME,
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::new(DEFAULT_PAGE_SIZE, DEFAULT_IO_QUEUE_PAIRS).with_intx_irq(),
    )
    .map_err(|err| OnProbeError::other(format!("failed to discover NVMe: {err:?}")))?;
    let endpoint = probe.take_endpoint();
    let irq_lease = PciIntxIrqLease::new(endpoint, binding);
    let irq = probe
        .into_platform_device()
        .register_irq_bound_block(driver, irq_lease);
    info!("NVMe block device registered at {address} with irq={irq:?}");
    Ok(())
}

fn register_msix_block(
    mut probe: ProbePci<'_>,
    bar: core::ops::Range<usize>,
    irq_lease: PciIrqLease,
) -> Result<Option<usize>, OnProbeError> {
    let vectors = irq_lease.vector_indices();

    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    // The MSI-X lease must own the endpoint across every later fallible step.
    // Its Drop path can then mask and disable the capability before releasing
    // the table mapping or provider vectors when staged discovery fails.
    let endpoint = probe.take_endpoint();
    let irq_lease = irq_lease.retain_endpoint(endpoint);

    let driver = NvmeBlockDriver::discover(
        DEVICE_NAME,
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::new(DEFAULT_PAGE_SIZE, DEFAULT_IO_QUEUE_PAIRS).with_msix_vectors(vectors),
    )
    .map_err(|err| OnProbeError::other(format!("failed to discover NVMe: {err:?}")))?;

    let plat_dev = probe.into_platform_device();
    Ok(plat_dev.register_irq_bound_block(driver, irq_lease))
}
