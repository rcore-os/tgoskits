use core::sync::atomic::{AtomicBool, Ordering};

use kernutil::StaticCell;
use loongarch_intc_driver::{EioVector, PchInput, PchPicConfig, PchPicCpuInterface, PchPicParts};
use rdif_intc::{AcpiGsiController, Interface};
use rdrive::{
    PlatformDevice, module_driver,
    probe::{OnProbeError, acpi::AcpiPchPic},
    register::{ProbeAcpi, ProbeFdt},
};

use crate::{
    common::ioremap,
    irq_routing::{
        CascadeTransitionError, PchPicFirmwareCount, PchPicInputCountSource,
        apply_parent_first_transition, pch_pic_input_count_source,
    },
    setup::MmioRaw,
};

const DEFAULT_PCH_PIC_SIZE: usize = 0x400;

struct PchRuntime {
    domain: crate::irq::IrqDomainId,
    cpu_interface: PchPicCpuInterface,
}

static RUNTIME: StaticCell<PchRuntime> = StaticCell::uninit();
static REGISTERED: AtomicBool = AtomicBool::new(false);

module_driver!(
    name: "Loongson PCH-PIC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,ls7a-pch-pic",
                "loongson,pch-pic-1.0",
                "loongson,pch-pic",
            ],
            on_probe: probe_pch_pic_fdt
        },
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_pch_pic_acpi
        },
    ],
);

pub fn irq_for_external_vector(vector: EioVector) -> Option<rdif_intc::IrqId> {
    let runtime = runtime()?;
    let input = runtime.cpu_interface.input_for_external_vector(vector)?;
    Some(rdif_intc::IrqId::new(
        runtime.domain,
        rdif_intc::HwIrq(input.raw() as u32),
    ))
}

/// Applies a PCH-PIC local enable transition with its parent EIO vector.
///
/// The PCH lock used to derive the immutable vector is released before the
/// EIO controller is changed. The local PCH lock is acquired only afterwards,
/// so the transaction never nests two `rdrive` controller locks. If the local
/// step fails, the parent is restored to its previous state.
pub fn set_irq_enabled(irq: crate::irq::IrqId, enabled: bool) -> Result<(), rdif_intc::IrqError> {
    let vector = external_vector_for_irq(irq)?;
    match apply_parent_first_transition(
        enabled,
        |state| super::eiointc::set_vector_enabled(vector, state),
        |state| crate::irq::set_controller_irq_enabled(irq, state),
    ) {
        Ok(()) => Ok(()),
        Err(CascadeTransitionError::Parent(error) | CascadeTransitionError::Local(error)) => {
            Err(error)
        }
        Err(CascadeTransitionError::Rollback { local, rollback }) => {
            warn!(
                "failed to roll back EIOINTC vector {vector:?} after PCH-PIC error {local:?}: \
                 {rollback:?}"
            );
            Err(rdif_intc::IrqError::Controller)
        }
    }
}

pub fn resolve_acpi_route(
    route: &rdif_intc::AcpiGsiRoute,
) -> Result<rdif_intc::IrqId, rdif_intc::IrqError> {
    let intc = pch_pic_controller_for_route(route)?;
    let mut intc = intc.try_lock().map_err(|_| rdif_intc::IrqError::Busy)?;
    if !intc.supports_acpi_gsi(route) {
        return Err(rdif_intc::IrqError::Unsupported);
    }
    let translation = intc.translate_acpi(route)?;
    intc.configure_acpi(&translation, route)?;
    Ok(translation.id)
}

fn runtime() -> Option<&'static PchRuntime> {
    REGISTERED.load(Ordering::Acquire).then(|| &*RUNTIME)
}

fn external_vector_for_irq(irq: crate::irq::IrqId) -> Result<EioVector, rdif_intc::IrqError> {
    let intc = crate::irq::intc_by_domain(irq.domain)?;
    let intc = intc
        .downcast::<loongarch_intc_driver::PchPicController>()
        .map_err(|_| rdif_intc::IrqError::InvalidIrq)?;
    let intc = intc.try_lock().map_err(|_| rdif_intc::IrqError::Busy)?;
    let input = PchInput::new(irq.hwirq.0 as usize).map_err(|_| rdif_intc::IrqError::InvalidIrq)?;
    intc.external_vector_for_input(input)
        .map_err(|_| rdif_intc::IrqError::InvalidIrq)
}

fn probe_pch_pic_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let reg = info
        .node
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no reg", info.node.name())))?;
    let base_vector = info
        .node
        .as_node()
        .get_property("loongson,pic-base-vec")
        .and_then(|property| property.get_u32())
        .unwrap_or(0) as usize;
    let vector_count = info
        .node
        .as_node()
        .get_property("loongson,pic-num-vecs")
        .and_then(|property| property.get_u32())
        .map(|count| count as usize);
    let mmio = ioremap(
        reg.address,
        reg.size.unwrap_or(DEFAULT_PCH_PIC_SIZE as u64) as usize,
    )
    .map_err(|error| OnProbeError::other(format!("failed to map PCH-PIC: {error:?}")))?;
    let input_count = vector_count.map_or(PchPicInputCountSource::HardwareId, |count| {
        pch_pic_input_count_source(PchPicFirmwareCount::ExplicitInputCount(count))
    });
    let config = pch_pic_config(&mmio, base_vector, 0, input_count)
        .map_err(|error| OnProbeError::other(format!("invalid PCH-PIC config: {error}")))?;

    register_pch_pic(dev, mmio, config)
}

fn probe_pch_pic_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let (info, dev) = probe.into_parts();
    let mut registered = false;

    for pch_pic in info.root.routing().pch_pics() {
        register_acpi_pch_pic(
            PlatformDevice {
                descriptor: dev.descriptor.clone(),
            },
            *pch_pic,
        )?;
        registered = true;
    }

    if registered {
        Ok(())
    } else {
        Err(OnProbeError::NotMatch)
    }
}

fn register_acpi_pch_pic(dev: PlatformDevice, info: AcpiPchPic) -> Result<(), OnProbeError> {
    let size = if info.mmio_size == 0 {
        DEFAULT_PCH_PIC_SIZE
    } else {
        usize::from(info.mmio_size)
    };
    let mmio = ioremap(info.address, size)
        .map_err(|error| OnProbeError::other(format!("failed to map ACPI PCH-PIC: {error:?}")))?;
    let input_count = pch_pic_input_count_source(PchPicFirmwareCount::AcpiGsiRoutingSpan(
        usize::from(info.gsi_count),
    ));
    let config = pch_pic_config(&mmio, 0, info.id, input_count)
        .map_err(|error| OnProbeError::other(format!("invalid ACPI PCH-PIC config: {error}")))?;
    register_pch_pic(dev, mmio, config)
}

fn pch_pic_config(
    mmio: &MmioRaw,
    base_vector: usize,
    controller_id: u16,
    input_count: PchPicInputCountSource,
) -> Result<PchPicConfig, loongarch_intc_driver::IntcError> {
    match input_count {
        PchPicInputCountSource::HardwareId => {
            PchPicConfig::detect(mmio, base_vector, controller_id)
        }
        PchPicInputCountSource::Explicit(count) => {
            PchPicConfig::new(base_vector, count, controller_id)
        }
    }
}

fn register_pch_pic(
    dev: PlatformDevice,
    mmio: MmioRaw,
    config: PchPicConfig,
) -> Result<(), OnProbeError> {
    let PchPicParts {
        controller,
        cpu_interface,
    } = PchPicParts::new(mmio, config)
        .map_err(|error| OnProbeError::other(format!("failed to initialize PCH-PIC: {error}")))?;
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchPchPic,
    )
    .map_err(|error| {
        OnProbeError::other(format!("failed to register PCH-PIC domain: {error:?}"))
    })?;

    dev.register(rdif_intc::Intc::new(domain, controller));
    if !RUNTIME.is_init() {
        RUNTIME.init(PchRuntime {
            domain,
            cpu_interface,
        });
        // Publish only after the value is fully initialized.
        REGISTERED.store(true, Ordering::Release);
    } else {
        warn!("additional Loongson PCH-PIC registered without a hard-IRQ CPU interface");
    }
    Ok(())
}

fn pch_pic_controller_for_route(
    route: &rdif_intc::AcpiGsiRoute,
) -> Result<rdrive::Device<rdif_intc::Intc>, rdif_intc::IrqError> {
    if route.controller != AcpiGsiController::PchPic {
        return Err(rdif_intc::IrqError::Unsupported);
    }
    if !rdrive::is_initialized() {
        return Err(rdif_intc::IrqError::Controller);
    }

    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let Ok(pic) = intc.downcast::<loongarch_intc_driver::PchPicController>() else {
            continue;
        };
        let guard = pic.try_lock().map_err(|_| rdif_intc::IrqError::Busy)?;
        let supported = guard.supports_acpi_gsi(route);
        drop(guard);
        if supported {
            return Ok(intc);
        }
    }

    warn!(
        "Loongson PCH-PIC is not registered for ACPI route controller={:?} address={:#x} input={}",
        route.controller, route.controller_address, route.controller_input
    );
    Err(rdif_intc::IrqError::Unsupported)
}
