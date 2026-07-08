use log::debug;
use rdif_intc::*;
use rdrive::{
    probe::OnProbeError,
    register::{DriverRegister, ProbeFdt, ProbeKind, ProbeLevel, ProbePriority},
};

pub struct IrqTest {}

pub fn register() -> DriverRegister {
    DriverRegister {
        name: "IrqTest",
        probe_kinds: &[ProbeKind::Fdt {
            compatibles: &["arm,cortex-a15-gic"],
            on_probe: probe_intc,
        }],
        level: ProbeLevel::PreKernel,
        priority: ProbePriority::INTC,
    }
}

impl rdrive::DriverGeneric for IrqTest {
    fn name(&self) -> &str {
        "IrqTest"
    }
}

impl Interface for IrqTest {
    fn translate_fdt(&self, irq_prop: &[u32]) -> Result<ControllerIrqTranslation, IrqError> {
        debug!("IrqTest translate_fdt: {:?}", irq_prop);
        Ok(ControllerIrqTranslation::new(HwIrq(42)))
    }
}

fn probe_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (fdt, plat_dev) = probe.into_parts();
    debug!(
        "on_probe: {}, parent intc {:?}",
        fdt.node.name(),
        plat_dev.descriptor.irq_parent,
    );
    plat_dev.register(Intc::new(IrqDomainId(0), IrqTest {}));

    Ok(())
}
