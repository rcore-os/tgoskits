use log::debug;
use rdrive::{
    PlatformDevice,
    driver::{Intc, systick::*},
    get,
    probe::OnProbeError,
    register::{DriverRegister, FdtInfo, ProbeKind, ProbeLevel, ProbePriority},
};

struct Timer;

pub fn register() -> DriverRegister {
    DriverRegister {
        name: "TimerTest",
        probe_kinds: &[ProbeKind::Fdt {
            compatibles: &["arm,pl031"],
            on_probe: probe,
        }],
        level: ProbeLevel::PreKernel,
        priority: ProbePriority::DEFAULT,
    }
}

fn probe(_node: FdtInfo<'_>, dev: PlatformDevice) -> Result<(), OnProbeError> {
    if let Some(parent) = dev.descriptor.irq_parent
        && let Ok(intc) = get::<Intc>(parent)
    {
        debug!("intc : {}", intc.descriptor().name);
    }

    dev.register_systick(Timer {});

    Ok(())
}

impl DriverGeneric for Timer {
    fn open(&mut self) -> Result<(), KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), KError> {
        Ok(())
    }
}

impl Interface for Timer {
    fn cpu_local(&mut self) -> local::Boxed {
        todo!()
    }
}
