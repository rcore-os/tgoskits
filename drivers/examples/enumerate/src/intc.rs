use log::debug;
use rdif_intc::*;
use rdrive::{
    PlatformDevice,
    probe::OnProbeError,
    register::{DriverRegister, FdtInfo, ProbeKind, ProbeLevel, ProbePriority},
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

fn probe_intc(fdt: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    debug!(
        "on_probe: {}, parent intc {:?}",
        fdt.node.name(),
        plat_dev.descriptor.irq_parent,
    );
    plat_dev.register(Intc::new(IrqTest {}));

    Ok(())
}
