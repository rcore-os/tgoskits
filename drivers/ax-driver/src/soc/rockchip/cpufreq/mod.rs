// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! RK3588 CPU DVFS hardware adapter for the shared `rdif-cpufreq` interface.
//!
//! A CPU OPP couples an SCMI PVTPLL clock request with its regulator voltage
//! and GRF read margin. The probe confirms bootstrap clocks and rails before
//! the shared runtime starts its policy worker. After all CPUs are online,
//! OTP, PVTM, temperature, and the board DT select each domain's OPP table.
//!
//! The three CPU domains and their SCMI clock IDs are:
//!
//! | cluster        | SCMI clock id | bootstrap MHz |
//! |----------------|---------------|---------------|
//! | A55 (little)   | 0             | 1008          |
//! | A76 big pair 0 | 2             | 1200          |
//! | A76 big pair 1 | 3             | 1200          |
//!
//! A `PostKernel` CPU-node probe runs before secondary CPUs start. The
//! `ax-runtime` worker serializes every later OPP request and thermal refresh.

use alloc::{format, vec::Vec};
#[cfg(feature = "rk3588-cpufreq-thermal-test")]
use core::sync::atomic::AtomicI32;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use ax_lazyinit::OnceLock;
use fdt_edit::{Fdt, NodeType, Phandle};
use log::{info, warn};
use rockchip_soc::rk3588::{
    cpufreq as soc_cpufreq,
    cpufreq_opp::{self, HardwareSelection},
};

use crate::{probe::OnProbeError, register::ProbeFdt, soc::scmi};

mod board;
mod margin;
mod pmic_i2c;
mod pmic_spi;
mod pvtm;
mod rdif;
mod selection;
mod sensors;
mod transition;

use margin::{GrfResource, ReadMargin};
use selection::configure_pvtpll_low_temp;
pub use selection::initialize_post_boot;
use transition::{align_rail_voltages_to_opp, apply_opp, read_mhz, set_and_verify};

/// SCMI clock id of the A55 (little) cluster — cpu0..3.
const A55_CLK_ID: u32 = 0;
/// SCMI clock ids of the two A76 (big) cluster pairs — cpu4/5 and cpu6/7. Both
/// must be set; they cover different core pairs.
const A76_CLK_IDS: [u32; 2] = [2, 3];

/// Confirmed A55 bootstrap rate, before OTP and PVTM authorize higher OPPs.
const A55_MAX_HZ: u64 = 1_008_000_000;
/// Confirmed A76 bootstrap rate, before OTP and PVTM authorize higher OPPs.
const A76_MAX_HZ: u64 = 1_200_000_000;
const A55_PVTM_HZ: u64 = 1_416_000_000;
const A76_PVTM_HZ: u64 = 1_608_000_000;
/// One-shot guard: several `cpu@*` nodes match, but the reclock runs once.
static APPLIED: AtomicBool = AtomicBool::new(false);
/// Exact SCMI clock-provider identity referenced by the RK3588 CPU nodes.
static SCMI_CLOCK_PHANDLE: AtomicU32 = AtomicU32::new(0);

crate::model_register!(
    name: "RK3588 CPU DVFS SCMI",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["arm,cortex-a55", "arm,cortex-a76"],
            on_probe: probe
        }
    ],
);

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    // Several CPU nodes match this driver; only the first invocation reclocks.
    if APPLIED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let phandle = probe.info().clocks().and_then(|clocks| {
        clocks
            .into_iter()
            .next()
            .map(|clock| clock.phandle)
            .ok_or_else(|| OnProbeError::other("RK3588 CPU node has no SCMI clock reference"))
    });
    rdif::register(probe.into_platform_device());
    let phandle = phandle?;
    match SCMI_CLOCK_PHANDLE.compare_exchange(0, phandle.raw(), Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => {}
        Err(existing) if existing == phandle.raw() => {}
        Err(existing) => {
            return Err(OnProbeError::other(format!(
                "RK3588 CPU clocks reference multiple SCMI providers: {existing:#x} and {}",
                phandle.raw()
            )));
        }
    }
    // Safety preflight (read-only): confirm this exact provider services all
    // CPU-cluster clocks before touching any of them. If any target id is
    // rejected, leave every cluster at its boot rate and bail.
    for id in [A55_CLK_ID, A76_CLK_IDS[0], A76_CLK_IDS[1]] {
        if scmi::clock_rate(phandle, id).is_none() {
            warn!(
                "cpufreq: SCMI does not service CPU cluster clock id {id}; leaving all CPU \
                 clusters at their boot rate (no DVFS applied)"
            );
            return Ok(());
        }
    }

    let a55_before = read_mhz(phandle, A55_CLK_ID);
    if !set_and_verify(phandle, A55_CLK_ID, A55_MAX_HZ, A55_MAX_HZ) {
        warn!("cpufreq: A55 reclock did not verify; stopping (A76 left at boot rate)");
        return Ok(());
    }
    let a55_after = read_mhz(phandle, A55_CLK_ID);

    let a76_before = read_mhz(phandle, A76_CLK_IDS[0]);
    for id in A76_CLK_IDS {
        if !set_and_verify(phandle, id, A76_MAX_HZ, A76_MAX_HZ) {
            warn!("cpufreq: A76 cluster clock id {id} reclock did not verify; stopping");
            return Ok(());
        }
    }
    let a76_after = read_mhz(phandle, A76_CLK_IDS[0]);

    info!("cpufreq: A55 {a55_before}->{a55_after}, A76 {a76_before}->{a76_after} MHz");

    // The CPU clock is voltage-coupled (proven on-board: at a fixed SCMI clock the
    // A76 runs ~1.19 GHz @675 mV but ~1.49 GHz @800 mV), so the clocks set above
    // overshoot while each rail sits at its ~800 mV boot value. Lower each rail to
    // its OPP nominal to pull the coupled clock onto the exact requested rate. Runs
    // LAST — after the SCMI clock is confirmed at target — so the down-shift is the
    // final step (matches Linux's reduce-freq-then-voltage order for
    // down-transitions), and only on the full-success path above.
    align_rail_voltages_to_opp();

    Ok(())
}

fn scmi_clock_phandle() -> Option<Phandle> {
    let phandle = SCMI_CLOCK_PHANDLE.load(Ordering::Acquire);
    (phandle != 0).then(|| Phandle::from(phandle))
}

fn rail_voltage(cluster: Cluster) -> Option<u32> {
    match cluster {
        Cluster::A55 => pmic_spi::get_uv(),
        Cluster::Big0 => pmic_i2c::get_uv(pmic_i2c::RK8602_BIG0_ADDR),
        Cluster::Big1 => pmic_i2c::get_uv(pmic_i2c::RK8603_BIG1_ADDR),
    }
}

fn grf_resource(fdt: &Fdt, phandle: u32) -> Result<GrfResource, FrequencyError> {
    let node = fdt
        .get_by_phandle(Phandle::from(phandle))
        .ok_or(FrequencyError::NotReady)?;
    let reg = node
        .regs()
        .into_iter()
        .next()
        .ok_or(FrequencyError::NotReady)?;
    Ok(GrfResource {
        address: reg.address,
        size: reg.size.ok_or(FrequencyError::NotReady)?,
    })
}

fn disable_all_domains() {
    for ready in &DOMAIN_READY {
        ready.store(false, Ordering::Release);
    }
    SELECTED_OPPS.call_once(|| [None, None, None]);
}

fn mark_domain_failed(_domain: FrequencyDomain) {
    // A big clock may have changed even when its readback failed. Its unknown
    // DSU floor also makes subsequent A55 transitions unsafe.
    for ready in &DOMAIN_READY {
        ready.store(false, Ordering::Release);
    }
}

// ===========================================================================
// Boot OPPs and selected silicon OPPs
// ===========================================================================

/// An OPP's requested SCMI rate, required rail voltage, and nominal frequency.
#[derive(Clone, Copy)]
struct Opp {
    ring_khz: u32,
    uv: u32,
    mhz: u32,
}

/// Bootstrap A76 ladder; only the confirmed 1200 MHz row is published before
/// the silicon-specific DT OPP table has been validated.
const A76_OPPS: &[Opp] = &[
    Opp {
        ring_khz: 408_000,
        uv: 750_000,
        mhz: 408,
    },
    Opp {
        ring_khz: 816_000,
        uv: 750_000,
        mhz: 816,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 750_000,
        mhz: 1200,
    },
];

/// Single confirmed fallback for an unselected big-cluster OPP table.
const A76_LIMITED_FALLBACK: &[Opp] = &[Opp {
    ring_khz: 816_000,
    uv: 750_000,
    mhz: 816,
}];

/// Bootstrap A55 ladder. Higher rows require RK806 selector readback and the
/// silicon-specific DT OPP selection.
const A55_OPPS: &[Opp] = &[
    Opp {
        ring_khz: 408_000,
        uv: 750_000,
        mhz: 408,
    },
    Opp {
        ring_khz: 816_000,
        uv: 750_000,
        mhz: 816,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 750_000,
        mhz: 1008,
    },
];

/// Index of the confirmed bootstrap OPP in each fallback ladder.
const BOOT_OPP_IDX: usize = 2;

// A table is published only after OTP, PVTM, the thermal sensor, regulator
// readback, and the board OPP properties have all been validated. A selection
// failure limits a big cluster to a confirmed low ring; a hardware transition
// failure disables all domains because their DSU relationship may be unknown.
struct SelectedDomain {
    opps: Vec<Opp>,
    margin: ReadMargin,
    low_temp_firmware: bool,
}

static SELECTED_OPPS: OnceLock<[Option<SelectedDomain>; 3]> = OnceLock::new();
static LIMITED_FALLBACK: [AtomicBool; 3] = [const { AtomicBool::new(false) }; 3];
static FLOOR_IDX: [AtomicUsize; 3] = [const { AtomicUsize::new(BOOT_OPP_IDX) }; 3];
static TEMPERATURE_STATE: [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];
#[cfg(feature = "rk3588-cpufreq-thermal-test")]
static TEST_TEMPERATURE_MC: AtomicI32 = AtomicI32::new(i32::MIN);

/// Adds a test temperature constraint without masking the live TSADC result.
/// `None` restores the sensor-only policy. Available only in board test builds.
#[cfg(feature = "rk3588-cpufreq-thermal-test")]
pub fn set_test_cpu_temperature_mc(temperature_mc: Option<i32>) {
    let value = temperature_mc.unwrap_or(i32::MIN);
    assert!(value == i32::MIN || (-40_000..=120_000).contains(&value));
    TEST_TEMPERATURE_MC.store(value, Ordering::Release);
}

/// The three DVFS domains (one little cluster, two big pairs).
#[derive(Clone, Copy)]
enum Cluster {
    A55,
    Big0,
    Big1,
}

/// A physical CPU frequency domain. The two A76 pairs have independent clocks
/// and regulators, so callers must address them separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrequencyDomain {
    Little,
    Big0,
    Big1,
}

impl FrequencyDomain {
    const fn index(self) -> usize {
        match self {
            Self::Little => 0,
            Self::Big0 => 1,
            Self::Big1 => 2,
        }
    }

    const fn cluster(self) -> Cluster {
        match self {
            Self::Little => Cluster::A55,
            Self::Big0 => Cluster::Big0,
            Self::Big1 => Cluster::Big1,
        }
    }
}

/// One confirmed nominal frequency and its supply requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperatingPoint {
    /// Nominal OPP frequency from the board DT.
    pub frequency_hz: u64,
    /// SCMI clock rate requested and read back for this OPP.
    pub ring_hz: u64,
    /// Confirmed regulator set point for this OPP.
    pub voltage_uv: u32,
}

/// Frequency bounds currently exposed by the driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrequencyLimits {
    pub min_hz: u64,
    pub max_hz: u64,
}

/// CPU frequency control errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrequencyError {
    NotReady,
    OppUnavailable,
    HardwareFailure,
}

/// The domain names are stable across supported operating systems.
pub const DOMAINS: [FrequencyDomain; 3] = [
    FrequencyDomain::Little,
    FrequencyDomain::Big0,
    FrequencyDomain::Big1,
];

fn describe(opp: Opp) -> OperatingPoint {
    OperatingPoint {
        frequency_hz: opp.mhz as u64 * 1_000_000,
        ring_hz: opp.ring_khz as u64 * 1_000,
        voltage_uv: opp.uv,
    }
}

fn effective_voltage(cluster: Cluster, nominal_uv: u32) -> u32 {
    soc_cpufreq::ThermalState::from_bits(TEMPERATURE_STATE[cluster.index()].load(Ordering::Acquire))
        .effective_voltage_uv(nominal_uv)
}

fn describe_for(domain: FrequencyDomain, opp: Opp) -> OperatingPoint {
    let mut point = describe(opp);
    point.voltage_uv = effective_voltage(domain.cluster(), opp.uv);
    point
}

fn check_ready(domain: FrequencyDomain) -> Result<(), FrequencyError> {
    if !driver_ready()
        || !DOMAIN_READY[domain.index()].load(Ordering::Acquire)
        || domain == FrequencyDomain::Little && !A55_RAIL_CONFIRMED.load(Ordering::Acquire)
    {
        Err(FrequencyError::NotReady)
    } else {
        Ok(())
    }
}

/// Returns the OPPs that this board driver can currently verify and apply.
pub fn available_opps(domain: FrequencyDomain) -> Result<Vec<OperatingPoint>, FrequencyError> {
    check_ready(domain)?;
    let range = allowed_indices(domain)?;
    Ok(domain.cluster().opps()[range]
        .iter()
        .copied()
        .map(|opp| describe_for(domain, opp))
        .collect())
}

/// Returns the last fully confirmed OPP for a domain.
pub fn current_opp(domain: FrequencyDomain) -> Result<OperatingPoint, FrequencyError> {
    check_ready(domain)?;
    let index = IDX[domain.index()].load(Ordering::Acquire);
    Ok(describe_for(domain, domain.cluster().opps()[index]))
}

/// Returns the current driver limits for a domain.
pub fn limits(domain: FrequencyDomain) -> Result<FrequencyLimits, FrequencyError> {
    check_ready(domain)?;
    let opps = domain.cluster().opps();
    let range = allowed_indices(domain)?;
    Ok(FrequencyLimits {
        min_hz: describe(opps[*range.start()]).frequency_hz,
        max_hz: describe(opps[*range.end()]).frequency_hz,
    })
}

fn allowed_indices(
    domain: FrequencyDomain,
) -> Result<core::ops::RangeInclusive<usize>, FrequencyError> {
    let minimum = minimum_index(domain).ok_or(FrequencyError::NotReady)?;
    let maximum = maximum_index(domain);
    (minimum <= maximum)
        .then_some(minimum..=maximum)
        .ok_or(FrequencyError::NotReady)
}

fn minimum_index(domain: FrequencyDomain) -> Option<usize> {
    let floor = FLOOR_IDX[domain.index()].load(Ordering::Acquire);
    if domain != FrequencyDomain::Little {
        return Some(floor);
    }
    let required = [FrequencyDomain::Big0, FrequencyDomain::Big1]
        .into_iter()
        .map(|big| {
            let current = IDX[big.index()].load(Ordering::Acquire);
            soc_cpufreq::dsu_minimum_hz(describe(big.cluster().opps()[current]).frequency_hz)
        })
        .max()
        .unwrap_or(0);
    domain
        .cluster()
        .opps()
        .iter()
        .position(|opp| u64::from(opp.mhz) * 1_000_000 >= required)
        .map(|index| index.max(floor))
}

fn maximum_index(domain: FrequencyDomain) -> usize {
    let opps = domain.cluster().opps();
    let state = soc_cpufreq::ThermalState::from_bits(
        TEMPERATURE_STATE[domain.index()].load(Ordering::Acquire),
    );
    let thermal_cap = state.maximum_hz(domain == FrequencyDomain::Little);
    let dsu_cap = if domain != FrequencyDomain::Little {
        if check_ready(FrequencyDomain::Little).is_err() {
            A76_MAX_HZ
        } else {
            let little = FrequencyDomain::Little.cluster().opps();
            let little_max = little[maximum_index(FrequencyDomain::Little)].mhz as u64 * 1_000_000;
            opps.iter()
                .rev()
                .find(|opp| soc_cpufreq::dsu_minimum_hz(opp.mhz as u64 * 1_000_000) <= little_max)
                .map_or(A76_MAX_HZ, |opp| opp.mhz as u64 * 1_000_000)
        }
    } else {
        u64::MAX
    };
    opps.iter()
        .rposition(|opp| opp.mhz as u64 * 1_000_000 <= thermal_cap.min(dsu_cap))
        .unwrap_or(BOOT_OPP_IDX)
}

/// Refresh BSP 10/15 C voltage floor and 85/80 C frequency cap. Only the
/// runtime worker calls this function, serially with all OPP transitions.
pub fn refresh_limits() -> Result<(), FrequencyError> {
    initialize_post_boot();
    let mut first_error = None;
    let mut previous = [soc_cpufreq::ThermalState::default(); 3];
    let temperature = sensors::soc_temperature_millidegrees().ok();
    for domain in [
        FrequencyDomain::Big0,
        FrequencyDomain::Big1,
        FrequencyDomain::Little,
    ] {
        let index = domain.index();
        let old =
            soc_cpufreq::ThermalState::from_bits(TEMPERATURE_STATE[index].load(Ordering::Acquire));
        let state = old.update(temperature);
        #[cfg(feature = "rk3588-cpufreq-thermal-test")]
        let state = {
            let mut constrained = state;
            if constrained.valid {
                let injected = TEST_TEMPERATURE_MC.load(Ordering::Acquire);
                if injected != i32::MIN {
                    let test_state = old.update(Some(injected));
                    constrained.low |= test_state.low;
                    constrained.high |= test_state.high;
                }
            }
            constrained
        };
        previous[index] = old;
        TEMPERATURE_STATE[index].store(state.bits(), Ordering::Release);
    }
    for domain in [
        FrequencyDomain::Big0,
        FrequencyDomain::Big1,
        FrequencyDomain::Little,
    ] {
        let index = domain.index();
        let old = previous[index];
        let state =
            soc_cpufreq::ThermalState::from_bits(TEMPERATURE_STATE[index].load(Ordering::Acquire));
        if check_ready(domain).is_err() {
            continue;
        }
        let firmware_low_temp = SELECTED_OPPS
            .get()
            .and_then(|tables| tables[index].as_ref())
            .is_some_and(|selected| selected.low_temp_firmware);
        if firmware_low_temp
            && old.low
            && !state.low
            && !configure_pvtpll_low_temp(domain.cluster().clock_id(), false)
        {
            mark_domain_failed(domain);
            first_error.get_or_insert(FrequencyError::HardwareFailure);
            continue;
        }
        let current = IDX[index].load(Ordering::Acquire);
        let cap = maximum_index(domain);
        let adjustment = if current > cap {
            let target = describe(domain.cluster().opps()[cap]).frequency_hz;
            set_frequency(domain, target)
        } else if old != state {
            let opp = domain.cluster().opps()[current];
            let previous_uv = old.effective_voltage_uv(opp.uv);
            let new_uv = effective_voltage(domain.cluster(), opp.uv);
            if previous_uv != new_uv && !apply_opp(domain.cluster(), opp, new_uv > previous_uv) {
                Err(FrequencyError::HardwareFailure)
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        if let Err(error) = adjustment {
            // The new thermal bound cannot be published while the clock may
            // still exceed it, even if no hardware write was attempted.
            mark_domain_failed(domain);
            first_error.get_or_insert(error);
            continue;
        }
        if firmware_low_temp
            && !old.low
            && state.low
            && !configure_pvtpll_low_temp(domain.cluster().clock_id(), true)
        {
            mark_domain_failed(domain);
            first_error.get_or_insert(FrequencyError::HardwareFailure);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Applies a confirmed OPP. The shared runtime serializes all callers with the
/// governor before entering this sleepable driver path.
pub fn set_frequency(domain: FrequencyDomain, frequency_hz: u64) -> Result<(), FrequencyError> {
    check_ready(domain)?;
    let cluster = domain.cluster();
    let opps = cluster.opps();
    let target = opps
        .iter()
        .position(|opp| describe(*opp).frequency_hz == frequency_hz)
        .ok_or(FrequencyError::OppUnavailable)?;
    if !allowed_indices(domain)?.contains(&target) {
        return Err(FrequencyError::OppUnavailable);
    }
    if domain != FrequencyDomain::Little {
        let required = soc_cpufreq::dsu_minimum_hz(frequency_hz);
        let little = FrequencyDomain::Little;
        let current = IDX[little.index()].load(Ordering::Acquire);
        if describe(little.cluster().opps()[current]).frequency_hz < required {
            check_ready(little)?;
            let boost = little.cluster().opps()[allowed_indices(little)?]
                .iter()
                .find(|opp| u64::from(opp.mhz) * 1_000_000 >= required)
                .ok_or(FrequencyError::OppUnavailable)?;
            set_frequency(little, describe(*boost).frequency_hz)?;
        }
    }
    let old = IDX[domain.index()].load(Ordering::Acquire);
    if target == old {
        return Ok(());
    }
    if !apply_opp(cluster, opps[target], target > old) {
        mark_domain_failed(domain);
        return Err(FrequencyError::HardwareFailure);
    }
    IDX[domain.index()].store(target, Ordering::Release);
    Ok(())
}

impl Cluster {
    fn index(self) -> usize {
        match self {
            Self::A55 => 0,
            Self::Big0 => 1,
            Self::Big1 => 2,
        }
    }

    fn opps(self) -> &'static [Opp] {
        if let Some(selected) = SELECTED_OPPS.get()
            && let Some(opps) = selected[self.index()].as_ref()
        {
            return &opps.opps;
        }
        match self {
            Cluster::A55 => A55_OPPS,
            _ if LIMITED_FALLBACK[self.index()].load(Ordering::Acquire) => A76_LIMITED_FALLBACK,
            _ => A76_OPPS,
        }
    }

    /// SCMI clock id feeding this domain's PVTPLL ring.
    fn clock_id(self) -> u32 {
        match self {
            Cluster::A55 => A55_CLK_ID,
            Cluster::Big0 => A76_CLK_IDS[0],
            Cluster::Big1 => A76_CLK_IDS[1],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Cluster::A55 => "A55",
            Cluster::Big0 => "A76b0",
            Cluster::Big1 => "A76b1",
        }
    }

    /// Set and read back this domain's rail voltage, including intermediate steps.
    /// RK806 supplies A55 over SPI; RK8602 and RK8603 supply the big clusters over I2C.
    fn set_voltage(self, uv: u32) -> bool {
        match self {
            Cluster::A55 => pmic_spi::set_uv_stepped_verified(uv),
            Cluster::Big0 => pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8602_BIG0_ADDR, uv),
            Cluster::Big1 => pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8603_BIG1_ADDR, uv),
        }
    }
}

/// Set once both big-cluster boot rails have been confirmed.
static GOV_READY: AtomicBool = AtomicBool::new(false);
/// A failed transition can leave a safe but only partly applied state.
static DOMAIN_READY: [AtomicBool; 3] = [const { AtomicBool::new(true) }; 3];
/// The RK806 path must read back the boot rail before publishing A55 OPPs.
static A55_RAIL_CONFIRMED: AtomicBool = AtomicBool::new(false);

/// Per-cluster current OPP index (A55, big0, big1), starting on the boot OPP the
/// voltage lever pinned.
static IDX: [AtomicUsize; 3] = [const { AtomicUsize::new(BOOT_OPP_IDX) }; 3];

fn driver_ready() -> bool {
    GOV_READY.load(Ordering::Acquire)
}

/// Reads the SCMI cluster clock from one `/cpus` node. A missing reference
/// cannot authorize a silicon-specific OPP table.
fn cpu_node_clock_id(node: &NodeType<'_>, phandle: Phandle) -> Option<u32> {
    node.clocks()
        .into_iter()
        .find(|clock| clock.phandle == phandle)
        .and_then(|clock| clock.specifier.first().copied())
        .or_else(|| {
            warn!(
                "cpufreq: cpu node {} has no recognizable SCMI cluster clock; it will not drive \
                 the governor",
                node.name()
            );
            None
        })
}
