//! AHCI discovery and controller-bundle registration.
//!
//! One registered bundle owns the HBA-wide initialization, IRQ endpoint, and
//! DMA lifecycle. Every identified ATA port is extracted later as an isolated
//! logical device, so the runtime never treats different disks as queues of a
//! single logical address space.

extern crate alloc;

use alloc::{format, vec};
use core::{any::Any, num::NonZeroUsize};

use ahci_host::{AhciConfig, AhciHost};
#[cfg(feature = "ahci")]
use pcie::CommandRegister;
use rdif_block::{
    BlkError, BlockIrqSource, BundleError, ControllerBundle, ControllerInitEndpoint, DriverGeneric,
    IrqSourceList, LifecycleEndpoint, LogicalDevice, LogicalDeviceId, LogicalDeviceIds,
};
use rdrive::probe::OnProbeError;
#[cfg(feature = "ahci")]
use rdrive::probe::pci::{FnOnProbe, ProbePci};
#[cfg(feature = "ls2k1000-ahci")]
use rdrive::register::ProbeFdt;

#[cfg(any(feature = "ahci", feature = "ls2k1000-ahci"))]
use super::PlatformDeviceBlock;
#[cfg(feature = "ls2k1000-ahci")]
use crate::binding_info_from_fdt;
#[cfg(feature = "ahci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint, pci::PciIntxIrqLease};

pub const DEVICE_NAME: &str = "ahci";
#[cfg(feature = "ls2k1000-ahci")]
const LS2K1000_DEVICE_NAME: &str = "ls2k1000-ahci";
const AHCI_LOGICAL_DEVICE_NAME: &str = "ahci-disk";
const LOGICAL_IRQ_SOURCE: usize = 0;

#[cfg(feature = "ahci")]
crate::model_register!(
    name: "AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci as FnOnProbe,
    }],
);

#[cfg(feature = "ls2k1000-ahci")]
crate::model_register!(
    name: "LS2K1000 AHCI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &[
            "loongson,ls-ahci",
            "loongson,ls2k1000-ahci",
            "loongson,2k1000-ahci",
            "generic-ahci",
            "snps,dwc-ahci",
        ],
        on_probe: probe_fdt,
    }],
);

struct AhciControllerBundle {
    host: AhciHost,
}

impl AhciControllerBundle {
    const fn new(host: AhciHost) -> Self {
        Self { host }
    }
}

impl DriverGeneric for AhciControllerBundle {
    fn name(&self) -> &str {
        self.host.name()
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl ControllerBundle for AhciControllerBundle {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        self.host.controller_init()
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        self.host.lifecycle()
    }

    fn logical_device_ids(&self) -> LogicalDeviceIds {
        LogicalDeviceIds::from_bits(self.host.available_port_ids().bits())
    }

    fn take_logical_device(
        &mut self,
        device_id: LogicalDeviceId,
        _max_queues: NonZeroUsize,
    ) -> Result<LogicalDevice, BundleError> {
        let port = device_id.get();
        if !self.host.available_port_ids().contains(port) {
            return Err(BundleError::DeviceUnavailable { device_id });
        }

        let logical_device_name = format!("{}-port{port}", self.host.name());
        let mut port_device = self
            .host
            .take_port_device(port, AHCI_LOGICAL_DEVICE_NAME)
            .map_err(|_| BundleError::DeviceUnavailable { device_id })?;
        let device_info = port_device.device_info();
        let queue_limits = port_device.queue_limits();
        let queue = port_device
            .create_queue()
            .ok_or(BundleError::NoQueues { device_id })?;
        Ok(LogicalDevice::new(
            device_id,
            logical_device_name,
            device_info,
            queue_limits,
            vec![queue],
        ))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        self.host.enable_irq()
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        self.host.disable_irq()
    }

    fn is_irq_enabled(&self) -> bool {
        self.host.is_irq_enabled()
    }

    fn irq_sources(&self) -> IrqSourceList {
        self.host.irq_sources()
    }

    fn take_irq_source(&mut self, source_id: usize) -> Option<BlockIrqSource> {
        self.host.take_irq_source(source_id)
    }
}

#[cfg(feature = "ahci")]
fn probe_pci(mut probe: ProbePci<'_>) -> Result<(), OnProbeError> {
    let class = probe.endpoint().revision_and_class();
    if (class.base_class, class.sub_class) != (0x01, 0x06) {
        return Err(OnProbeError::NotMatch);
    }
    let bar = probe
        .endpoint()
        .bar_mmio(5)
        .or_else(|| probe.endpoint().bar_mmio(0))
        .ok_or_else(|| OnProbeError::other("AHCI MMIO BAR missing"))?;
    let binding = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    PciIntxIrqLease::mask_for_discovery(probe.endpoint_mut());
    probe.endpoint_mut().update_command(|mut command| {
        command.insert(
            CommandRegister::MEMORY_ENABLE
                | CommandRegister::BUS_MASTER_ENABLE
                | CommandRegister::INTERRUPT_DISABLE,
        );
        command
    });

    let host = discover_host(DEVICE_NAME, bar.start, bar.count().max(1))?;
    let endpoint = probe.take_endpoint();
    let irq_lease = PciIntxIrqLease::new(endpoint, binding);
    let registered = probe
        .into_platform_device()
        .register_irq_bound_controller_bundle(AhciControllerBundle::new(host), irq_lease);
    log::info!("registered AHCI controller bundle: slot={registered:?}");
    Ok(())
}

#[cfg(feature = "ls2k1000-ahci")]
fn probe_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let register = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.path())))?;
    let size = register
        .size
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| OnProbeError::other("AHCI MMIO register size is missing or too large"))?;
    let address = usize::try_from(register.address)
        .map_err(|_| OnProbeError::other("AHCI MMIO address does not fit usize"))?;
    let binding = binding_info_from_fdt(&info)?;
    if binding.irq_for_source(LOGICAL_IRQ_SOURCE).is_none() {
        return Err(OnProbeError::other(
            "AHCI controller has no interrupt source",
        ));
    }

    let host = discover_host(LS2K1000_DEVICE_NAME, address, size)?;
    let registered =
        platform.register_controller_bundle_with_info(AhciControllerBundle::new(host), binding);
    log::info!("registered LS2K1000 AHCI controller bundle: slot={registered:?}");
    Ok(())
}

fn discover_host(
    name: &'static str,
    address: usize,
    size: usize,
) -> Result<AhciHost, OnProbeError> {
    AhciHost::discover(
        name,
        address,
        size,
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        AhciConfig::legacy_irq(LOGICAL_IRQ_SOURCE),
    )
    .map_err(|error| OnProbeError::other(format!("failed to discover {name}: {error}")))
}
