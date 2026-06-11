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
    fn setup_irq_by_fdt(&mut self, irq_prop: &[u32]) -> IrqId {
        debug!("IrqTest setup_irq_by_fdt: {:?}", irq_prop);
        42.into()
    }
}

fn probe_intc(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (fdt, plat_dev) = probe.into_parts();
    debug!(
        "on_probe: {}, parent intc {:?}",
        fdt.node.name(),
        plat_dev.descriptor.irq_parent,
    );
    plat_dev.register(Intc::new(IrqTest {}));

    Ok(())
}
