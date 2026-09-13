use core::sync::atomic::{AtomicBool, Ordering};

use kernutil::StaticCell;
use loongarch_intc_driver::{
    EioIntcConfig, EioIntcCpuInterface, EioIntcParts, EioVector, NativeIocsr,
};
use rdrive::{
    PlatformDevice, module_driver,
    probe::OnProbeError,
    register::{ProbeAcpi, ProbeFdt},
};

const EIOINTC_CPU_IRQ: usize = 3;
const EIOINTC_VECTOR_COUNT: usize = 256;

type CpuInterface = EioIntcCpuInterface<NativeIocsr>;

struct EioRuntime {
    domain: crate::irq::IrqDomainId,
    cpu_interface: CpuInterface,
}

static RUNTIME: StaticCell<EioRuntime> = StaticCell::uninit();
static REGISTERED: AtomicBool = AtomicBool::new(false);

module_driver!(
    name: "Loongson EIOINTC",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::INTC,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,ls2k2000-eiointc",
                "loongson,ls3a5000-eiointc",
                "loongson,eiointc",
            ],
            on_probe: probe_eiointc_fdt
        },
        ProbeKind::Acpi {
            ids: &[],
            on_probe: probe_eiointc_acpi
        },
    ],
);

pub fn set_vector_enabled(vector: EioVector, enabled: bool) -> Result<(), rdif_intc::IrqError> {
    let runtime = runtime().ok_or(rdif_intc::IrqError::Controller)?;
    crate::irq::set_controller_irq_enabled(
        crate::irq::IrqId::new(runtime.domain, crate::irq::HwIrq(vector.raw() as u32)),
        enabled,
    )
}

pub fn claim_irq() -> Option<EioVector> {
    runtime().and_then(|runtime| runtime.cpu_interface.claim())
}

pub fn complete_irq(vector: EioVector) {
    let Some(runtime) = runtime() else {
        return;
    };
    if let Err(error) = runtime.cpu_interface.complete(vector) {
        warn!("ignore invalid EIOINTC completion {vector:?}: {error}");
    }
}

pub fn irq_id(vector: EioVector) -> Option<crate::irq::IrqId> {
    runtime().map(|runtime| {
        crate::irq::IrqId::new(runtime.domain, crate::irq::HwIrq(vector.raw() as u32))
    })
}

fn runtime() -> Option<&'static EioRuntime> {
    REGISTERED.load(Ordering::Acquire).then(|| &*RUNTIME)
}

fn probe_eiointc_fdt(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    register_eiointc(probe.into_platform_device())
}

fn probe_eiointc_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    if probe.info().root.routing().pch_pics().is_empty() {
        return Err(OnProbeError::NotMatch);
    }
    register_eiointc(probe.into_platform_device())
}

fn register_eiointc(dev: PlatformDevice) -> Result<(), OnProbeError> {
    if REGISTERED.load(Ordering::Acquire) {
        return Err(OnProbeError::other(
            "Loongson EIOINTC is already registered",
        ));
    }

    let config = EioIntcConfig::new(EIOINTC_VECTOR_COUNT)
        .map_err(|error| OnProbeError::other(format!("invalid EIOINTC config: {error}")))?;
    let EioIntcParts {
        controller,
        cpu_interface,
    } = EioIntcParts::new(NativeIocsr, config)
        .map_err(|error| OnProbeError::other(format!("failed to initialize EIOINTC: {error}")))?;
    let domain = crate::irq::alloc_irq_domain(
        dev.descriptor.device_id(),
        crate::irq::IrqDomainKind::LoongArchEioIntc,
    )
    .map_err(|error| {
        OnProbeError::other(format!("failed to register EIOINTC domain: {error:?}"))
    })?;

    dev.register(rdif_intc::Intc::new(domain, controller));
    RUNTIME.init(EioRuntime {
        domain,
        cpu_interface,
    });
    // Publish controller, domain, and CPU interface before enabling the CPU
    // cascade line that makes hard IRQ dispatch reachable.
    REGISTERED.store(true, Ordering::Release);
    super::boot_irq_set_enable(someboot::irq::IrqId::new(EIOINTC_CPU_IRQ), true);
    Ok(())
}
