use alloc::{format, string::ToString, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

use arm_scmi_rs::{MSG_HEADER_WORD, PAYLOAD_WORD, Scmi, ScmiError, Shmem, Smc};
use ax_lazyinit::OnceLock;
use ax_sync::RawSpinLock;
use fdt_edit::Phandle;
use log::{info, warn};

use crate::{DriverGeneric, KError, mmio::iomap, probe::OnProbeError, register::ProbeFdt};

const SCMI_SHMEM_SIZE: usize = 0x100;
const RK3588_SCMI_SHMEM_BASE: usize = 0x10f000;
const SCMI_CLOCK_PROTOCOL_ID: u32 = 0x14;

type ScmiAgent = Arc<RawSpinLock<Scmi<Smc>>>;

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
    let agent = Arc::new(RawSpinLock::new(Scmi::new(Smc::new(smc_id, None), shmem)));
    // Kept for the guest relay: the host serializes the single shared channel
    // and answers the guest's CPU-clock requests itself.
    CHANNEL_AGENT.call_once(|| agent.clone());
    SMC_ID.call_once(|| smc_id);
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

/// The probe's SCMI agent, used to serialize every transaction on the single
/// shared channel, including the ones relayed for a guest.
static CHANNEL_AGENT: OnceLock<ScmiAgent> = OnceLock::new();
/// The platform's SCMI SMC function identifier from the device tree.
static SMC_ID: OnceLock<u32> = OnceLock::new();
/// Number of guest CPU-clock writes answered by the host, so the log reports
/// the first few and stays quiet for a guest that retries.
static DENIED_WRITES: AtomicUsize = AtomicUsize::new(0);

/// SCMI protocol and message identifiers used by the CPU-clock ownership rule.
const SCMI_CLOCK_PROTOCOL: u32 = 0x14;
const SCMI_CLOCK_RATE_SET: u32 = 0x05;
const SCMI_CLOCK_CONFIG_SET: u32 = 0x07;
/// SCMI clock identifiers of the three CPU frequency domains.
const CPU_CLOCK_IDS: [u32; 3] = [0, 2, 3];
/// The clock identifier a message addresses, if it carries one.
///
/// `CLOCK_RATE_SET` carries `flags, clock_id, rate` while `CLOCK_CONFIG_SET`
/// carries `clock_id, attributes`, so only the rate request leads with flags.
fn message_clock_id(message: u32, words: &[u32; 9]) -> Option<u32> {
    let offset = match message {
        SCMI_CLOCK_RATE_SET => PAYLOAD_WORD + 1,
        SCMI_CLOCK_CONFIG_SET => PAYLOAD_WORD,
        _ => return None,
    };
    words.get(offset).copied()
}

/// Relays one guest SMC that carries an SCMI request, or answers it here.
///
/// The host owns CPU DVFS, but the platform has a single SCMI shared channel
/// that the passthrough guest also writes to. Two agents using that channel
/// concurrently corrupt each other's messages, so the host takes the channel
/// lock for both its own transactions and the guest's relayed ones, and it
/// answers the guest's CPU-clock *writes* itself instead of letting the guest
/// program the same clock the host governor owns.
///
/// Returns `None` when the call is not an SCMI request for this channel, which
/// leaves the platform's normal forwarding path in charge.
pub fn relay_guest_smc(x0: u64, x1: u64, x2: u64, x3: u64) -> Option<(u64, u64, u64, u64)> {
    let smc_id = *SMC_ID.get()?;
    if x0 as u32 != smc_id {
        return None;
    }
    let agent = CHANNEL_AGENT.get()?;
    // The plain `arm,scmi-smc` binding sends no buffer argument, so any call
    // with this function id carries SCMI traffic for the one shared channel.
    // A guest that fills the parameter registers (the `arm,scmi-smc-param` or
    // Qualcomm bindings) addresses a different channel and stays on the
    // platform's own forwarding path.
    if x1 != 0 || x2 != 0 {
        return None;
    }
    let agent = agent.lock_irqsave();
    let words = agent.debug_words();
    let header = words[MSG_HEADER_WORD];
    // Protocol id occupies bits 17:10 of the message header, message id bits
    // 7:0, exactly as `arm-scmi-rs` encodes them.
    let protocol = (header >> 10) & 0xff;
    let message = header & 0xff;
    let clock_id = (protocol == SCMI_CLOCK_PROTOCOL)
        .then(|| message_clock_id(message, &words))
        .flatten();
    if clock_id.is_some_and(|clock_id| CPU_CLOCK_IDS.contains(&clock_id)) {
        if DENIED_WRITES.fetch_add(1, Ordering::Relaxed) < 3 {
            warn!(
                "SCMI relay: guest CPU clock write denied, message={message:#x}, clock={}; the \
                 host owns CPU DVFS",
                clock_id.unwrap_or_default()
            );
        }
        agent.publish_response(0);
        return Some((0, x1, x2, x3));
    }
    Some(agent.relay_smc(x0, x1, x2, x3))
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
    set_clock_rate_checked(phandle, clock_id, rate).ok()
}

/// Preserve the SCMI rejection reason for board-specific flagged rate requests.
pub fn set_clock_rate_checked(phandle: Phandle, clock_id: u32, rate: u64) -> Result<(), KError> {
    let provider =
        clock_provider(phandle).ok_or(KError::Unknown("SCMI clock provider unavailable"))?;
    let mut provider = provider.lock().map_err(|_| KError::Busy)?;
    provider
        .set_rate(rdif_clk::ClockId::from(clock_id as usize), rate)
        .map_err(|error| {
            warn!(
                "SCMI clock rate set failed: provider={phandle}, clock_id={clock_id:#x}, \
                 rate={rate} Hz, {error:?}"
            );
            error
        })
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
        let agent = self.agent.lock_irqsave();
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
            .lock_irqsave()
            .clock_rate_get_direct(clock_id)
            .map_err(|error| {
                warn!("SCMI clock rate get failed: clock_id={clock_id:#x}, {error:?}");
                KError::Io
            })
    }

    fn set_rate(&mut self, id: rdif_clk::ClockId, rate: u64) -> Result<(), KError> {
        let clock_id = clock_id(id)?;
        self.agent
            .lock_irqsave()
            .clock_rate_set_direct(clock_id, rate)
            .map_err(|error| {
                warn!(
                    "SCMI clock rate set failed: clock_id={clock_id:#x}, rate={rate} Hz, {error:?}"
                );
                match error {
                    ScmiError::InvalidParameters => KError::InvalidArg { name: "rate" },
                    _ => KError::Io,
                }
            })
    }
}

fn clock_id(id: rdif_clk::ClockId) -> Result<u32, KError> {
    u32::try_from(id.raw()).map_err(|_| KError::InvalidArg { name: "clock_id" })
}
