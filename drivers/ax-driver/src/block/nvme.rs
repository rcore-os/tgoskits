extern crate alloc;

use alloc::{boxed::Box, format};

use log::{info, warn};
use nvme_driver::{Config, Nvme, NvmeBlockDriver};
use pcie::{CommandRegister, DeviceType};
use rdif_block::{
    BQueue, DeviceInfo, Interface, IrqHandler, IrqSourceList, QueueHandle, QueueLimits,
};
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{
    PciIrqRequirement,
    block::{PlatformDeviceBlock, ProbePciBlock},
    pci::PciMsixAllocation,
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
        PciMsixAllocation::allocate(endpoint, info, DEFAULT_IO_QUEUE_PAIRS as u16)
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
        Config::new(DEFAULT_PAGE_SIZE, DEFAULT_IO_QUEUE_PAIRS).with_intx_irq(),
    )
    .map_err(|err| OnProbeError::other(format!("failed to initialize NVMe: {err:?}")))?;
    let driver = NvmeBlockDriver::from_nvme(nvme).map_err(|err| {
        OnProbeError::other(format!("failed to create NVMe block driver: {err:?}"))
    })?;
    let irq = probe.register_block(driver, PciIrqRequirement::Required)?;
    info!("NVMe block device registered at {address} with irq={irq:?}");
    Ok(())
}

fn register_msix_block(
    mut probe: ProbePci<'_>,
    bar: core::ops::Range<usize>,
    msix: PciMsixAllocation,
) -> Result<Option<usize>, OnProbeError> {
    let irq_info = msix.binding_info();
    let vectors = msix.vector_indices();

    probe.endpoint_mut().update_command(|mut cmd| {
        cmd.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        cmd
    });

    let nvme = Nvme::new(
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::new(DEFAULT_PAGE_SIZE, DEFAULT_IO_QUEUE_PAIRS).with_msix_vectors(vectors),
    )
    .map_err(|err| OnProbeError::other(format!("failed to initialize NVMe: {err:?}")))?;
    let driver = NvmeBlockDriver::from_nvme(nvme).map_err(|err| {
        OnProbeError::other(format!("failed to create NVMe block driver: {err:?}"))
    })?;

    let (_, _, plat_dev) = probe.into_parts();
    Ok(plat_dev.register_block_with_info(MsixBlockDriver::new(driver, msix), irq_info))
}

struct MsixBlockDriver<T> {
    inner: T,
    msix: PciMsixAllocation,
}

impl<T> MsixBlockDriver<T> {
    const fn new(inner: T, msix: PciMsixAllocation) -> Self {
        Self { inner, msix }
    }
}

impl<T: Interface> rdif_block::DriverGeneric for MsixBlockDriver<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        self.inner.raw_any()
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        self.inner.raw_any_mut()
    }
}

impl<T: Interface> Interface for MsixBlockDriver<T> {
    fn device_info(&self) -> DeviceInfo {
        self.inner.device_info()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.inner.queue_limits()
    }

    fn create_queue(&mut self) -> Option<BQueue> {
        self.inner.create_queue()
    }

    fn create_owned_queue(&mut self) -> Option<QueueHandle> {
        self.inner.create_owned_queue()
    }

    fn enable_irq(&self) {
        self.msix.enable();
        self.inner.enable_irq();
    }

    fn disable_irq(&self) {
        self.inner.disable_irq();
        self.msix.disable();
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.inner.irq_sources()
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        self.inner.take_irq_handler(source_id)
    }
}
