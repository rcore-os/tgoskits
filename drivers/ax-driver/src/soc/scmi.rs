use alloc::{format, string::ToString, sync::Arc};

use arm_scmi_rs::{Scmi, Shmem, Smc};
use ax_kspin::SpinNoIrq as Mutex;
use fdt_edit::Phandle;
use log::{info, warn};

use crate::{DriverGeneric, KError, mmio::iomap, probe::OnProbeError, register::ProbeFdt};

const SCMI_SHMEM_SIZE: usize = 0x100;
const RK3588_SCMI_SHMEM_BASE: usize = 0x10f000;
const SCMI_CLOCK_PROTOCOL_ID: u32 = 0x14;

type ScmiAgent = Arc<Mutex<Scmi<Smc>>>;

crate::model_register!(
    name: "ARM SCMI SMC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::CLK,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,scmi-smc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, plat_dev) = probe.into_parts();
    let smc_id = info
        .node
        .as_node()
        .get_property("arm,smc-id")
        .and_then(|prop| prop.get_u32())
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no arm,smc-id", info.node.name())))?;
    let shmem_phandle = info
        .node
        .as_node()
        .get_property("shmem")
        .and_then(|prop| prop.get_u32_iter().next())
        .ok_or_else(|| OnProbeError::other(format!("[{}] has no shmem", info.node.name())))?;
    let (shmem_addr, shmem_size) = info
        .node
        .regs()
        .into_iter()
        .next()
        .map(|reg| {
            (
                reg.address as usize,
                reg.size.unwrap_or(SCMI_SHMEM_SIZE as u64) as usize,
            )
        })
        .unwrap_or_else(|| {
            warn!(
                "[{}] SCMI shmem phandle {} cannot be resolved by rdrive; using RK3588 shmem \
                 fallback {:#x}+{:#x}",
                info.node.name(),
                shmem_phandle,
                RK3588_SCMI_SHMEM_BASE,
                SCMI_SHMEM_SIZE
            );
            (RK3588_SCMI_SHMEM_BASE, SCMI_SHMEM_SIZE)
        });
    let shmem_base = iomap(shmem_addr, shmem_size)?;

    let shmem = Shmem {
        address: shmem_base,
        bus_address: shmem_addr,
        size: shmem_size,
    };
    let agent = Arc::new(Mutex::new(Scmi::new(Smc::new(smc_id, None), shmem)));
    if let Some(clock_child) = clock_protocol_child(&info) {
        let clock_path = clock_child.path().to_string();
        let clock_phandle = clock_child.node().as_node().phandle();
        plat_dev
            .register_with_fdt_child(
                ScmiDevice {
                    _agent: agent.clone(),
                },
                clock_child,
                rdif_clk::Clk::new(ScmiClockProvider { agent }),
            )
            .map_err(|error| OnProbeError::other(error.to_string()))?;
        info!(
            "SCMI clock protocol registered: path={clock_path}, phandle={clock_phandle:?}, \
             protocol={:#x}",
            SCMI_CLOCK_PROTOCOL_ID
        );
    } else {
        plat_dev.register(ScmiDevice { _agent: agent });
        warn!("[{}] has no SCMI clock protocol", info.node.name());
    }
    info!(
        "SCMI SMC registered: smc_id={:#x}, shmem_phandle={}, shmem={:#x}+{:#x}",
        smc_id, shmem_phandle, shmem_addr, shmem_size
    );
    Ok(())
}

fn clock_protocol_child(
    info: &crate::register::FdtInfo<'_>,
) -> Option<rdrive::probe::fdt::FdtChild> {
    info.available_children().into_iter().find(|child| {
        child
            .node()
            .as_node()
            .get_property("reg")
            .and_then(|property| property.get_u32())
            == Some(SCMI_CLOCK_PROTOCOL_ID)
    })
}

pub fn clock_rate(phandle: Phandle, clock_id: u32) -> Option<u64> {
    let provider = clock_provider(phandle)?;
    let provider = provider.lock().ok()?;
    provider
        .get_rate(rdif_clk::ClockId::from(clock_id as usize))
        .map_err(|error| {
            warn!(
                "SCMI clock rate get failed: provider={phandle}, clock_id={clock_id:#x}, {error:?}"
            );
        })
        .ok()
}

pub fn enable_clock(phandle: Phandle, clock_id: u32) -> Option<()> {
    let provider = clock_provider(phandle)?;
    let mut provider = provider.lock().ok()?;
    provider
        .enable(rdif_clk::ClockId::from(clock_id as usize))
        .map_err(|error| {
            warn!(
                "SCMI clock enable failed: provider={phandle}, clock_id={clock_id:#x}, {error:?}"
            );
        })
        .ok()
}

pub fn set_clock_rate(phandle: Phandle, clock_id: u32, rate: u64) -> Option<()> {
    let provider = clock_provider(phandle)?;
    let mut provider = provider.lock().ok()?;
    provider
        .set_rate(rdif_clk::ClockId::from(clock_id as usize), rate)
        .map_err(|error| {
            warn!(
                "SCMI clock rate set failed: provider={phandle}, clock_id={clock_id:#x}, \
                 rate={rate} Hz, {error:?}"
            );
        })
        .ok()
}

fn clock_provider(phandle: Phandle) -> Option<rdrive::Device<rdif_clk::Clk>> {
    let Some(device_id) = rdrive::fdt_phandle_to_device_id(phandle) else {
        warn!("SCMI clock provider phandle {phandle} has no FDT device identity");
        return None;
    };
    rdrive::get::<rdif_clk::Clk>(device_id)
        .map_err(|error| {
            warn!("SCMI clock provider {phandle} is unavailable: {error}");
        })
        .ok()
}

struct ScmiDevice {
    _agent: ScmiAgent,
}

impl DriverGeneric for ScmiDevice {
    fn name(&self) -> &str {
        "arm-scmi-smc"
    }
}

struct ScmiClockProvider {
    agent: ScmiAgent,
}

impl DriverGeneric for ScmiClockProvider {
    fn name(&self) -> &str {
        "arm-scmi-clock"
    }
}

impl rdif_clk::Interface for ScmiClockProvider {
    fn perper_enable(&mut self) {}

    fn enable(&mut self, id: rdif_clk::ClockId) -> Result<(), KError> {
        let clock_id = clock_id(id)?;
        let agent = self.agent.lock();
        agent
            .protocol_clk_no_init()
            .clk_enable(clock_id)
            .map_err(|error| {
                warn!("SCMI clock enable failed: clock_id={clock_id:#x}, {error:?}");
                KError::Io
            })
    }

    fn get_rate(&self, id: rdif_clk::ClockId) -> Result<u64, KError> {
        let clock_id = clock_id(id)?;
        self.agent
            .lock()
            .clock_rate_get_direct(clock_id)
            .map_err(|error| {
                warn!("SCMI clock rate get failed: clock_id={clock_id:#x}, {error:?}");
                KError::Io
            })
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        let clock_id = clock_id(id)?;
        self.agent
            .lock()
            .clock_rate_set_direct(clock_id, rate)
            .map_err(|error| {
                warn!(
                    "SCMI clock rate set failed: clock_id={clock_id:#x}, rate={rate} Hz, {error:?}"
                );
                KError::Io
            })
    }
}

fn clock_id(id: rdif_clk::ClockId) -> Result<u32, KError> {
    u32::try_from(id.raw()).map_err(|_| KError::InvalidArg { name: "clock_id" })
}
