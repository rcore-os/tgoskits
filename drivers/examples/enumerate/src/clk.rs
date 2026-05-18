use log::debug;
use rdrive::{
    driver::clk::*,
    probe::OnProbeError,
    register::{DriverRegister, FdtInfo, ProbeKind, ProbeLevel, ProbePriority},
    *,
};

struct Clock {
    rate: u64,
}

pub fn register() -> DriverRegister {
    DriverRegister {
        name: "APB CLK",
        probe_kinds: &[ProbeKind::Fdt {
            compatibles: &["fixed-clock"],
            on_probe: probe,
        }],
        level: ProbeLevel::PreKernel,
        priority: ProbePriority::CLK,
    }
}

fn probe(_node: FdtInfo<'_>, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    plat_dev.register(Clk::new(Clock { rate: 0 }));

    Ok(())
}

impl DriverGeneric for Clock {
    fn open(&mut self) -> Result<(), KError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), KError> {
        Ok(())
    }
}

impl Interface for Clock {
    fn perper_enable(&mut self) {
        debug!("enable");
    }

    fn get_rate(&self, _id: ClockId) -> Result<u64, KError> {
        Ok(self.rate)
    }

    fn set_rate(&mut self, _id: ClockId, rate: u64) -> Result<(), KError> {
        self.rate = rate;
        Ok(())
    }
}

module_driver!(
    name: "TEST CLK",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[ProbeKind::Fdt {
            compatibles: &["test-clk"],
            on_probe: probe_clk,
        }],
);

fn probe_clk(_fdt: FdtInfo<'_>, _desc: PlatformDevice) -> Result<(), OnProbeError> {
    todo!()
}
