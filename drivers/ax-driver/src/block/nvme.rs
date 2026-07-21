extern crate alloc;

use alloc::{boxed::Box, format, vec::Vec};

use log::{info, warn};
use nvme_driver::{Config, NvmeBlockActivator};
use pcie::{CommandRegister, DeviceType, Endpoint};
use rdif_block::ActivationPlan;
use rdrive::probe::{
    OnProbeError,
    pci::{FnOnProbe, ProbePci},
};

use crate::{
    BindingInfo, PciIrqRequirement, binding_info_from_pci_endpoint,
    block::{
        PlatformDeviceBlockActivation, PlatformIrqActivationError, PlatformIrqActivationFailure,
        PlatformIrqActivator, RealizedPlatformIrqBinding,
    },
    pci::{PciIntxIrqLease, PciIrqLease, PciMsixActivationFailure, PciMsixPreflight},
};

pub const DEVICE_NAME: &str = "nvme";
const DEFAULT_PAGE_SIZE: usize = 0x1000;
const MAX_IO_QUEUE_PAIRS: u16 = u64::BITS as u16;

struct DeferredNvmeMsixBinding {
    preflight: PciMsixPreflight,
    endpoint: Endpoint,
}

impl PlatformIrqActivator for DeferredNvmeMsixBinding {
    fn discovery_binding(&self) -> &BindingInfo {
        self.preflight.discovery_binding()
    }

    fn realize(
        self: Box<Self>,
        plan: &ActivationPlan,
    ) -> Result<RealizedPlatformIrqBinding, PlatformIrqActivationFailure> {
        let Self {
            preflight,
            endpoint,
        } = *self;
        let Some(vector_count) = selected_msix_vector_count(plan) else {
            return Err(PlatformIrqActivationFailure::returned(
                PlatformIrqActivationError::InvalidPlan,
                Self {
                    preflight,
                    endpoint,
                },
            ));
        };
        match preflight.activate(endpoint, vector_count) {
            Ok(lease) => Ok(RealizedPlatformIrqBinding::new(lease)),
            Err(PciMsixActivationFailure::Returned { endpoint, error }) => {
                warn!("runtime-selected NVMe MSI-X activation rolled back: {error}");
                Err(PlatformIrqActivationFailure::returned(
                    PlatformIrqActivationError::Returned,
                    Self {
                        preflight,
                        endpoint,
                    },
                ))
            }
            Err(PciMsixActivationFailure::Claimed { error }) => {
                warn!("runtime-selected NVMe MSI-X activation entered quarantine: {error}");
                Err(PlatformIrqActivationFailure::quarantined())
            }
        }
    }
}

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

    let preflight = match PciIrqLease::preflight(probe.endpoint(), probe.info(), MAX_IO_QUEUE_PAIRS)
    {
        Ok(preflight) => preflight,
        Err(OnProbeError::Unsupported(reason)) => {
            info!("NVMe PCI endpoint {address} MSI-X unavailable ({reason}); using legacy INTx");
            return register_intx_block(probe, bar, address);
        }
        Err(err) => return Err(err),
    };

    let max_queue_pairs = preflight.max_vector_count();
    let activator = discover_msix_activator(bar.clone(), max_queue_pairs)?;
    probe.endpoint_mut().update_command(enable_nvme_command);
    let endpoint = probe.take_endpoint();
    let deferred_irq = DeferredNvmeMsixBinding {
        preflight,
        endpoint,
    };
    let irq = probe
        .into_platform_device()
        .register_deferred_irq_block_activator(activator, deferred_irq);
    if irq.is_none() {
        return Err(OnProbeError::claimed(format!(
            "NVMe deferred MSI-X owner at {address} could not enter the block registry"
        )));
    }
    info!(
        "NVMe block device registered at {address} with up to {max_queue_pairs} plan-selected \
         MSI-X vectors"
    );
    Ok(())
}

fn register_intx_block(
    mut probe: ProbePci<'_>,
    bar: core::ops::Range<usize>,
    address: rdrive::probe::pci::PciAddress,
) -> Result<(), OnProbeError> {
    let binding = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    PciIntxIrqLease::mask_for_discovery(probe.endpoint_mut());
    probe.endpoint_mut().update_command(enable_nvme_command);

    let activator = NvmeBlockActivator::discover(
        DEVICE_NAME,
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::new(DEFAULT_PAGE_SIZE, 1).with_intx_irq(),
    )
    .map_err(|err| OnProbeError::other(format!("failed to discover NVMe: {err:?}")))?;
    let endpoint = probe.take_endpoint();
    let irq_lease = PciIntxIrqLease::new(endpoint, binding);
    let irq = probe
        .into_platform_device()
        .register_irq_bound_block_activator(activator, irq_lease);
    info!("NVMe block device registered at {address} with irq={irq:?}");
    Ok(())
}

fn discover_msix_activator(
    bar: core::ops::Range<usize>,
    max_queue_pairs: u16,
) -> Result<NvmeBlockActivator, OnProbeError> {
    let vectors = (0..max_queue_pairs).collect::<Vec<_>>();
    NvmeBlockActivator::discover(
        DEVICE_NAME,
        bar.start,
        bar.count().max(1),
        u64::MAX,
        axklib::dma::op(),
        axklib::mmio::op(),
        Config::new(DEFAULT_PAGE_SIZE, usize::from(max_queue_pairs)).with_msix_vectors(vectors),
    )
    .map_err(|err| OnProbeError::other(format!("failed to discover NVMe: {err:?}")))
}

fn selected_msix_vector_count(plan: &ActivationPlan) -> Option<u16> {
    let mut source_bits = plan.control_activation().irq_sources().bits();
    let mut queue_count = 0_u16;
    for domain in plan.domains() {
        source_bits |= domain.irq_sources().bits();
        queue_count = queue_count.checked_add(domain.queue_count().get())?;
    }
    let vector_count = u16::try_from(source_bits.count_ones()).ok()?;
    if vector_count == 0 || vector_count != queue_count {
        return None;
    }
    let expected_bits = if vector_count == u64::BITS as u16 {
        u64::MAX
    } else {
        (1_u64 << vector_count) - 1
    };
    (source_bits == expected_bits).then_some(vector_count)
}

fn enable_nvme_command(mut command: CommandRegister) -> CommandRegister {
    command.insert(
        CommandRegister::MEMORY_ENABLE
            | CommandRegister::BUS_MASTER_ENABLE
            | CommandRegister::INTERRUPT_DISABLE,
    );
    command
}
