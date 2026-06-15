extern crate alloc;

use alloc::format;

use log::info;
use nvme_driver::{Config, Nvme, NvmeBlockDriver};
use pcie::{CommandRegister, DeviceType};
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{PciIrqRequirement, block::ProbePciBlock};

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
    let endpoint = probe.endpoint_mut();
    if endpoint.device_type() != DeviceType::NvmeController {
        return Err(OnProbeError::NotMatch);
    }

    let Some(bar) = endpoint.bar_mmio(0) else {
        return Err(OnProbeError::other("NVMe BAR0 MMIO missing"));
    };

    endpoint.update_command(|mut cmd| {
        cmd.insert(CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE);
        cmd.insert(CommandRegister::INTERRUPT_DISABLE);
        cmd
    });

    let address = endpoint.address();
    info!(
        "NVMe PCI endpoint {address}: BAR0={:#x}..{:#x}, int_pin={}, int_line={}",
        bar.start,
        bar.end,
        endpoint.interrupt_pin(),
        endpoint.interrupt_line()
    );

    let nvme = Nvme::new(
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config {
            page_size: DEFAULT_PAGE_SIZE,
            io_queue_pair_count: DEFAULT_IO_QUEUE_PAIRS,
        },
    )
    .map_err(|err| OnProbeError::other(format!("failed to initialize NVMe: {err:?}")))?;
    let driver = NvmeBlockDriver::from_nvme(nvme).map_err(|err| {
        OnProbeError::other(format!("failed to create NVMe block driver: {err:?}"))
    })?;
    let irq = probe.register_block(driver, PciIrqRequirement::Optional)?;
    info!("NVMe block device registered at {address} with irq {irq:?}");
    Ok(())
}
