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

use super::{
    cpufreq_margin::{GrfResource, ReadMargin},
    cpufreq_pvtm, cpufreq_sensors,
};
use crate::{probe::OnProbeError, register::ProbeFdt, soc::scmi};

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
    super::cpufreq_rdif::register(probe.into_platform_device());
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
    use super::{pmic_i2c, pmic_spi};
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

fn select_domain_opps(
    fdt: &Fdt,
    cluster: Cluster,
    phandle: Phandle,
    hold_measurement_clock: bool,
) -> Result<SelectedDomain, FrequencyError> {
    let cpu = fdt
        .find_compatible(&["arm,cortex-a55", "arm,cortex-a76"])
        .into_iter()
        .find(|node| cpu_node_clock_id(node, phandle) == Some(cluster.clock_id()))
        .ok_or(FrequencyError::NotReady)?;
    let table_phandle = cpu
        .as_node()
        .get_property("operating-points-v2")
        .and_then(|property| property.get_u32())
        .ok_or(FrequencyError::NotReady)?;
    let table = fdt
        .get_by_phandle(Phandle::from(table_phandle))
        .ok_or(FrequencyError::NotReady)?;
    let table_node = table.as_node();
    let property_u32 = |name| {
        table_node
            .get_property(name)
            .and_then(|property| property.get_u32())
            .ok_or(FrequencyError::NotReady)
    };

    let serial = cpufreq_sensors::sku_serial().map_err(|_| FrequencyError::NotReady)?;
    let bin = match serial {
        0x0d => 1,
        0x0a => 2,
        _ => 0,
    };
    if bin != 0 {
        warn!(
            "cpufreq: {} SKU bin {bin} has no board-validated high OPP; retaining boot OPP",
            cluster.name()
        );
        return Err(FrequencyError::NotReady);
    }
    let cell_phandle = table_node
        .get_property("nvmem-cells")
        .and_then(|property| property.get_u32_iter().nth(1))
        .ok_or(FrequencyError::NotReady)?;
    let cell = fdt
        .get_by_phandle(Phandle::from(cell_phandle))
        .ok_or(FrequencyError::NotReady)?;
    let cell_reg = cell
        .regs()
        .into_iter()
        .next()
        .ok_or(FrequencyError::NotReady)?;
    let opp_info = cpufreq_sensors::read_otp_bytes(
        usize::try_from(cell_reg.address).map_err(|_| FrequencyError::NotReady)?,
        usize::try_from(cell_reg.size.ok_or(FrequencyError::NotReady)?)
            .map_err(|_| FrequencyError::NotReady)?,
    )
    .map_err(|_| FrequencyError::NotReady)?;

    let temperature = cpufreq_sensors::cpu_temperature_millidegrees(cluster.index())
        .map_err(|_| FrequencyError::NotReady)?;
    let grf = grf_resource(fdt, property_u32("rockchip,grf")?)?;
    let measurement_hz = u64::from(property_u32("rockchip,pvtm-freq")?) * 1000;
    let measurement_uv = property_u32("rockchip,pvtm-volt")?;
    let delay_us = property_u32("rockchip,pvtm-sample-time")?;
    let offset = property_u32("rockchip,pvtm-offset")?;
    let margin_cells = table_node
        .get_property("volt-mem-read-margin")
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .ok_or(FrequencyError::NotReady)?;
    let dsu = table_node
        .get_property("rockchip,dsu-grf")
        .and_then(|property| property.get_u32())
        .map(|phandle| grf_resource(fdt, phandle))
        .transpose()?;
    let margin = ReadMargin::new(grf, dsu, &margin_cells).map_err(|_| FrequencyError::NotReady)?;
    let boot_hz = if matches!(cluster, Cluster::A55) {
        A55_MAX_HZ
    } else {
        A76_MAX_HZ
    };
    let expected_measurement_hz = if matches!(cluster, Cluster::A55) {
        A55_PVTM_HZ
    } else {
        A76_PVTM_HZ
    };
    if measurement_uv != 750_000
        || measurement_hz != expected_measurement_hz
        || rail_voltage(cluster) != Some(measurement_uv)
        || scmi::clock_rate(phandle, cluster.clock_id()) != Some(boot_hz)
        || delay_us == 0
        || delay_us > 10_000
    {
        return Err(FrequencyError::NotReady);
    }
    if !matches!(cluster, Cluster::A55)
        && scmi::clock_rate(phandle, A55_CLK_ID)
            .is_none_or(|rate| rate < soc_cpufreq::dsu_minimum_hz(measurement_hz))
    {
        return Err(FrequencyError::NotReady);
    }
    if margin.establish_for_voltage(measurement_uv).is_err() {
        DOMAIN_READY[cluster.index()].store(false, Ordering::Release);
        if matches!(cluster, Cluster::A55) {
            DOMAIN_READY[1].store(false, Ordering::Release);
            DOMAIN_READY[2].store(false, Ordering::Release);
        }
        return Err(FrequencyError::HardwareFailure);
    }
    if !set_and_verify(phandle, cluster.clock_id(), measurement_hz, measurement_hz) {
        DOMAIN_READY[cluster.index()].store(false, Ordering::Release);
        return Err(FrequencyError::HardwareFailure);
    }
    axklib::time::busy_wait(core::time::Duration::from_micros(u64::from(delay_us)));
    let sample = cpufreq_pvtm::read_raw_sample(grf.address, grf.size, offset);
    if !hold_measurement_clock
        && !set_and_verify(phandle, cluster.clock_id(), boot_hz, measurement_hz)
    {
        DOMAIN_READY[cluster.index()].store(false, Ordering::Release);
        return Err(FrequencyError::HardwareFailure);
    }
    let sample = sample.map_err(|_| FrequencyError::HardwareFailure)?;
    let temp_props = table_node
        .get_property("rockchip,pvtm-temp-prop")
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .ok_or(FrequencyError::NotReady)?;
    let [below, above] = temp_props.as_slice() else {
        return Err(FrequencyError::NotReady);
    };
    let corrected = cpufreq_pvtm::temperature_correct(
        sample,
        temperature,
        property_u32("rockchip,pvtm-ref-temp")? as i32,
        [*below as i32, *above as i32],
    )
    .ok_or(FrequencyError::NotReady)?;
    let pvtm_hw = property_u32("rockchip,pvtm-hw")?;
    let grade_property = if cpufreq_pvtm::uses_hardware_bin_table(bin, pvtm_hw) {
        "rockchip,pvtm-voltage-sel-hw"
    } else {
        "rockchip,pvtm-voltage-sel"
    };
    let grade_cells = table_node
        .get_property(grade_property)
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .ok_or(FrequencyError::NotReady)?;
    let (rows, remainder) = grade_cells.as_chunks::<3>();
    if !remainder.is_empty() {
        return Err(FrequencyError::NotReady);
    }
    let grade = u8::try_from(
        cpufreq_pvtm::select_voltage_grade(corrected, rows).ok_or(FrequencyError::NotReady)?,
    )
    .map_err(|_| FrequencyError::NotReady)?;
    let grade_measured = match cluster {
        Cluster::A55 => matches!(grade, 0 | 1),
        Cluster::Big0 | Cluster::Big1 => matches!(grade, 0 | 3),
    };
    if !grade_measured {
        warn!(
            "cpufreq: {} PVTM grade {grade} has no board-validated high OPP; retaining boot OPP",
            cluster.name()
        );
        return Err(FrequencyError::NotReady);
    }
    let selection = HardwareSelection::from_otp(serial, grade, &opp_info)
        .map_err(|_| FrequencyError::NotReady)?;
    let selected = cpufreq_opp::parse_domain_opps(fdt, cpu.as_node(), selection)
        .map_err(|_| FrequencyError::NotReady)?;
    let opps = selected
        .into_iter()
        .map(|opp| {
            let rail_ceiling_uv = if matches!(cluster, Cluster::A55) {
                950_000
            } else {
                1_000_000
            };
            if opp.cpu.target_uv > rail_ceiling_uv
                || opp
                    .memory
                    .is_some_and(|memory| memory.target_uv != opp.cpu.target_uv)
                || opp.frequency_hz % 1_000_000 != 0
                || opp.frequency_hz / 1_000_000 > u64::from(u32::MAX)
                || opp.frequency_hz / 1000 > u64::from(u32::MAX)
            {
                return Err(FrequencyError::NotReady);
            }
            Ok(Opp {
                ring_khz: (opp.frequency_hz / 1000) as u32,
                uv: opp.cpu.target_uv,
                mhz: (opp.frequency_hz / 1_000_000) as u32,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    info!(
        "cpufreq: {} OTP bin={} PVTM raw={} corrected={} grade={} temp={}mC, {} OPPs, max={}MHz",
        cluster.name(),
        bin,
        sample,
        corrected,
        grade,
        temperature,
        opps.len(),
        opps.last().map_or(0, |opp| opp.mhz)
    );
    Ok(SelectedDomain { opps, margin })
}

/// Complete silicon selection after device probing and CPU startup, before the
/// shared policy worker begins applying requests.
pub fn initialize_post_boot() {
    if SELECTED_OPPS.is_initialized() || !driver_ready() {
        return;
    }
    let Some(phandle) = scmi_clock_phandle() else {
        return;
    };
    let Some(fdt) = rdrive::fdt_ref() else {
        return;
    };
    let select = |cluster, hold| match select_domain_opps(fdt, cluster, phandle, hold) {
        Ok(selected) => Some(selected),
        Err(error) => {
            warn!(
                "cpufreq: {} OPP selection failed: {error:?}; keeping boot OPP",
                cluster.name()
            );
            None
        }
    };
    // The A55 PVTM measurement point is 1.416 GHz at 750 mV. Keep it
    // confirmed there while the big clusters sample at 1.608 GHz: their BSP
    // DSU dependency requires at least 1.2 GHz from the little domain.
    let little = select(Cluster::A55, true);
    let little_holds_dsu = little.is_some()
        && DOMAIN_READY[0].load(Ordering::Acquire)
        && rail_voltage(Cluster::A55) == Some(750_000)
        && scmi::clock_rate(phandle, A55_CLK_ID) == Some(A55_PVTM_HZ);
    let big0 = little_holds_dsu
        .then(|| select(Cluster::Big0, false))
        .flatten();
    let big1 = little_holds_dsu
        .then(|| select(Cluster::Big1, false))
        .flatten();
    // A failed PVTM clock SET can leave a big cluster at 1.608 GHz even when
    // its readback is unknown. Confirm both big rings at 1.2 GHz before
    // lowering A55 below their 1.2 GHz DSU requirement.
    let mut big_boot_confirmed = true;
    for id in A76_CLK_IDS {
        if scmi::clock_rate(phandle, id) != Some(A76_MAX_HZ)
            && !set_and_verify(phandle, id, A76_MAX_HZ, A76_PVTM_HZ)
        {
            big_boot_confirmed = false;
        }
    }
    if !big_boot_confirmed {
        warn!("cpufreq: big-cluster PVTM clock recovery failed; leaving A55 clock unchanged");
        disable_all_domains();
        return;
    }
    if !set_and_verify(phandle, A55_CLK_ID, A55_MAX_HZ, A55_PVTM_HZ) {
        disable_all_domains();
        return;
    }
    let selected = [little, big0, big1];
    let selected = SELECTED_OPPS.call_once(|| selected);
    for (index, domain) in DOMAINS.into_iter().enumerate() {
        let Some(table) = selected[index].as_ref() else {
            continue;
        };
        let boot_hz = if index == 0 { A55_MAX_HZ } else { A76_MAX_HZ };
        let Some(boot_index) = table
            .opps
            .iter()
            .position(|opp| opp.ring_khz as u64 * 1000 == boot_hz)
        else {
            mark_domain_failed(domain);
            break;
        };
        if !apply_opp(domain.cluster(), table.opps[boot_index], false) {
            mark_domain_failed(domain);
            break;
        }
        IDX[index].store(boot_index, Ordering::Release);
        FLOOR_IDX[index].store(0, Ordering::Release);
    }
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

/// Programs `clock_id` to `target`, but never above the caller's confirmed
/// rail-voltage ceiling, then verifies the platform actually applied it.
/// Returns `true` only when the read-back matches the request.
///
/// The ceiling is the boot ring during probing and the PVTM measurement ring
/// after the 750 mV rail is confirmed. A rejected set or a deviating read-back
/// returns `false`; the caller then stops because the actual clock may differ
/// from the requested rate.
fn set_and_verify(phandle: Phandle, clock_id: u32, target: u64, ceiling: u64) -> bool {
    if target > ceiling {
        warn!(
            "cpufreq: refusing to set clock id {clock_id} to {target} Hz (above confirmed ceiling \
             {ceiling} Hz)"
        );
        return false;
    }
    if scmi::set_clock_rate(phandle, clock_id, target).is_none() {
        warn!("cpufreq: SCMI rejected clock id {clock_id} set to {target} Hz; left unchanged");
        return false;
    }
    match scmi::clock_rate(phandle, clock_id) {
        Some(applied) if applied == target => true,
        Some(applied) if applied > ceiling => {
            warn!(
                "cpufreq: clock id {clock_id} read back {applied} Hz ABOVE confirmed ceiling \
                 {ceiling} Hz (requested {target} Hz); stopping"
            );
            false
        }
        Some(applied) => {
            warn!(
                "cpufreq: clock id {clock_id} read back {applied} Hz, requested {target} Hz; \
                 stopping"
            );
            false
        }
        None => {
            warn!("cpufreq: could not read back clock id {clock_id} after set; stopping");
            false
        }
    }
}

/// Current rate of `clock_id` in MHz, or 0 if it cannot be read (logging only).
fn read_mhz(phandle: Phandle, clock_id: u32) -> u64 {
    scmi::clock_rate(phandle, clock_id).unwrap_or(0) / 1_000_000
}

// ===========================================================================
// CPU-rail voltage alignment (the voltage half of the coupled clock)
// ===========================================================================

/// Master gate for the PMIC **writes**. While `false`, [`align_rail_voltages_to_opp`]
/// only *reads and logs* each rail's boot voltage (zero PMIC writes). Flipped to
/// `true` after the read-only board pass confirmed the A76 rails read the true
/// 800 mV boot voltage (i2c0 bring-up: ungate + reset + pinmux). Even with this
/// on, a rail is only lowered when its own read is trustworthy (see the A55 gate
/// below — its spi2/RK806 read is not up yet, so it is skipped).
const APPLY_RAIL_VOLTAGE: bool = true;

/// Conservative boot voltage shared by standard and J/M OPP rows. The standard
/// rows allow 675 mV, but OTP is not yet available here, so that row is closed.
// The J/M OPP rows require 750 mV at the same boot ring. Until OTP selection
// exists, never lower a rail to the standard-SKU-only 675 mV row.
const A76_NOMINAL_UV: u32 = 750_000;
const A55_NOMINAL_UV: u32 = 750_000;

/// Read (and, once validated, lower) the three CPU-cluster rails to their OPP
/// nominal so the voltage-coupled clock lands on the exact requested frequency.
///
/// Order matters: the A76 clusters are reclocked before `start_secondary_cpus()`
/// so no core is scheduled on them — their voltage is lowered first. The A55 rail
/// feeds the live boot core, so it is lowered last, and only via the stepped path
/// (the voltage-coupled clock tracks the rail down with no undervolt transient).
fn align_rail_voltages_to_opp() {
    use super::{pmic_i2c, pmic_spi};

    // --- A76 big0/big1 rails: RK8602 @0x42 / RK8603 @0x43 over I2C bus0 ---
    let a76_ok = pmic_i2c::init();
    if a76_ok {
        for (name, chip) in [
            ("big0", pmic_i2c::RK8602_BIG0_ADDR),
            ("big1", pmic_i2c::RK8603_BIG1_ADDR),
        ] {
            match pmic_i2c::get_uv(chip) {
                Some(uv) => info!("cpufreq: A76 {name} rail boot voltage = {uv} uV"),
                None => warn!("cpufreq: A76 {name} rail voltage read failed"),
            }
        }
    } else {
        warn!("cpufreq: A76 PMIC (I2C) init failed; A76 left at boot voltage");
    }

    // --- A55 (little) rail: RK806 DCDC2 over SPI2 ---
    let a55_ok = pmic_spi::init();
    if a55_ok {
        match pmic_spi::get_uv() {
            Some(uv) => info!("cpufreq: A55 rail boot voltage = {uv} uV"),
            None => warn!("cpufreq: A55 rail voltage read failed"),
        }
    } else {
        warn!("cpufreq: A55 PMIC (SPI) init failed; A55 left at boot voltage");
    }

    if !APPLY_RAIL_VOLTAGE {
        info!("cpufreq: rail-voltage alignment is READ-ONLY this build (no PMIC writes)");
        return;
    }

    // --- Apply: stepped, down-only, read-back-verified lower to a common safe row. ---
    let mut a76_aligned = false;
    if a76_ok {
        let b0 = pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8602_BIG0_ADDR, A76_NOMINAL_UV);
        let b1 = pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8603_BIG1_ADDR, A76_NOMINAL_UV);
        a76_aligned = b0 && b1;
        info!("cpufreq: A76 rails -> {A76_NOMINAL_UV} uV (big0 ok={b0}, big1 ok={b1})");
    }
    // A55 (spi2/RK806): only lower the rail after a trustworthy selector readback.
    // A real A55 boot voltage is in [675 mV, 950 mV] per the DCDC2 range and
    // cluster0 OPP table.
    match if a55_ok { pmic_spi::get_uv() } else { None } {
        Some(v) if (675_000..=950_000).contains(&v) => {
            let a55 = pmic_spi::set_uv_stepped_verified(A55_NOMINAL_UV);
            A55_RAIL_CONFIRMED.store(a55, Ordering::Release);
            info!("cpufreq: A55 rail {v} -> {A55_NOMINAL_UV} uV (ok={a55})");
        }
        other => warn!(
            "cpufreq: A55 boot voltage not trustworthy ({other:?} uV); leaving A55 DVFS \
             unavailable"
        ),
    }

    // The runtime may start only after both big-cluster rails are confirmed.
    // A55 remains unavailable independently if its RK806 rail readback failed.
    if a76_aligned {
        GOV_READY.store(true, Ordering::Release);
        info!("cpufreq: boot rails confirmed (A76 I2C up; a55_spi={a55_ok})");
    } else {
        warn!(
            "cpufreq: DVFS unavailable (a76_pmic={a76_ok}, aligned={a76_aligned}); clusters stay \
             on boot OPP"
        );
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
// failure retains the bootstrap ladder; a hardware transition failure disables
// all domains because their DSU relationship may no longer be known.
struct SelectedDomain {
    opps: Vec<Opp>,
    margin: ReadMargin,
}

static SELECTED_OPPS: OnceLock<[Option<SelectedDomain>; 3]> = OnceLock::new();
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
    use super::cpufreq_sensors;

    initialize_post_boot();
    let mut first_error = None;
    let mut previous = [soc_cpufreq::ThermalState::default(); 3];
    for domain in [
        FrequencyDomain::Big0,
        FrequencyDomain::Big1,
        FrequencyDomain::Little,
    ] {
        let index = domain.index();
        let old =
            soc_cpufreq::ThermalState::from_bits(TEMPERATURE_STATE[index].load(Ordering::Acquire));
        let temperature = cpufreq_sensors::cpu_temperature_millidegrees(index).ok();
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
        let current = IDX[index].load(Ordering::Acquire);
        let cap = maximum_index(domain);
        if current > cap {
            let target = describe(domain.cluster().opps()[cap]).frequency_hz;
            if let Err(error) = set_frequency(domain, target) {
                // The new thermal bound cannot be published while the clock
                // may still exceed it, even if no hardware write was attempted.
                mark_domain_failed(domain);
                first_error.get_or_insert(error);
            }
        } else if old != state {
            let opp = domain.cluster().opps()[current];
            let previous_uv = old.effective_voltage_uv(opp.uv);
            let new_uv = effective_voltage(domain.cluster(), opp.uv);
            if previous_uv != new_uv && !apply_opp(domain.cluster(), opp, new_uv > previous_uv) {
                mark_domain_failed(domain);
                first_error.get_or_insert(FrequencyError::HardwareFailure);
            }
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
            _ => &A76_OPPS[..=BOOT_OPP_IDX],
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
        use super::{pmic_i2c, pmic_spi};
        match self {
            Cluster::A55 => pmic_spi::set_uv_stepped_verified(uv),
            Cluster::Big0 => pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8602_BIG0_ADDR, uv),
            Cluster::Big1 => pmic_i2c::set_uv_stepped_verified(pmic_i2c::RK8603_BIG1_ADDR, uv),
        }
    }
}

/// Apply an OPP to a domain as a matched (voltage, frequency) pair, ordered so
/// the voltage-coupled clock never overshoots its rail:
///   - going UP:   raise voltage first, then the SCMI ring (clock follows up);
///   - going DOWN: lower the SCMI ring first, then voltage (clock follows down).
///
/// Stops at the first failed step and returns `true` only after both steps
/// have been read back. The caller then commits its software OPP index.
///
/// Each step is CONFIRMED, not just requested: the clock step reads the SCMI rate
/// back and requires it to equal the request (a SET ack alone is not proof the ring
/// switched), and the A76 voltage step reads back over I2C. So "ring set succeeds"
/// below means the ring is read-back-confirmed at the new rate — a downshift only
/// reaches its voltage-lower step once the clock is verified already lowered.
///
/// No-undervolt argument (both failure points, each direction):
///   - UPSHIFT, voltage step fails: the ring set is skipped entirely, so
///     nothing changed — old freq + old voltage, still a valid, previously
///     confirmed pairing.
///   - UPSHIFT, voltage step succeeds but the ring set fails: the rail is
///     already at (or above) `opp.uv` while the ring is still at its old,
///     lower value — over-volted for whatever it is currently delivering,
///     never under-volted.
///   - DOWNSHIFT, ring step fails: the voltage lower is skipped entirely, so
///     nothing changed — old freq + old voltage, still valid.
///   - DOWNSHIFT, ring step succeeds but the voltage step fails: the ring is
///     already at its new, lower value while the rail is still at its old,
///     higher voltage — over-volted for the new (lower) clock, never
///     under-volted.
#[must_use]
fn apply_opp(cluster: Cluster, opp: Opp, going_up: bool) -> bool {
    let Some(phandle) = scmi_clock_phandle() else {
        warn!("cpufreq: SCMI clock provider is not initialized");
        return false;
    };
    let hz = opp.ring_khz as u64 * 1_000;
    let target_uv = effective_voltage(cluster, opp.uv);
    soc_cpufreq::transition(going_up, |step| {
        let confirmed = match step {
            soc_cpufreq::TransitionStep::SupplyAndMargin => {
                let margin = SELECTED_OPPS
                    .get()
                    .and_then(|tables| tables[cluster.index()].as_ref())
                    .map(|selected| &selected.margin);
                let applied = if going_up {
                    cluster.set_voltage(target_uv)
                        && margin.is_none_or(|margin| margin.set_for_voltage(target_uv).is_ok())
                } else {
                    margin.is_none_or(|margin| margin.set_for_voltage(target_uv).is_ok())
                        && cluster.set_voltage(target_uv)
                };
                if applied {
                    return Ok(());
                }
                if going_up {
                    warn!(
                        "cpufreq: {} upshift to {} mV failed; leaving clock id {} unchanged (no \
                         undervolt: old freq stays paired with old voltage)",
                        cluster.name(),
                        opp.uv / 1_000,
                        cluster.clock_id()
                    );
                } else {
                    warn!(
                        "cpufreq: {} clock already lowered to {} Hz but voltage write to {} mV \
                         failed; not committing this OPP (safe: over-volted for the new, lower \
                         clock, never under-volted)",
                        cluster.name(),
                        hz,
                        opp.uv / 1_000
                    );
                }
                false
            }
            soc_cpufreq::TransitionStep::Clock => {
                let cid = cluster.clock_id();
                // A SCMI CLOCK_RATE_SET ack is NOT proof the PVTPLL ring actually
                // switched — read the rate back and require it to equal the request
                // before treating the clock step as confirmed (same read-back the boot
                // `set_and_verify` does). This is what makes a DOWNSHIFT safe: the
                // voltage step that follows only runs once the clock is CONFIRMED at its
                // new, lower rate, so we can never lower the rail under a still-high
                // clock (the high-freq/low-voltage window the ordering exists to prevent).
                let set_ok = scmi::set_clock_rate(phandle, cid, hz).is_some();
                let applied = if set_ok {
                    scmi::clock_rate(phandle, cid)
                } else {
                    None
                };
                if set_ok && applied == Some(hz) {
                    return Ok(());
                }
                if going_up {
                    warn!(
                        "cpufreq: {} upshift: SCMI clock id {cid} not confirmed at {hz} Hz \
                         (set_ok={set_ok}, read_back={applied:?}); not committing this OPP (safe: \
                         voltage already raised, over-volted for the old, lower clock, never \
                         under-volted)",
                        cluster.name()
                    );
                } else {
                    warn!(
                        "cpufreq: {} downshift: SCMI clock id {cid} not confirmed at {hz} Hz \
                         (set_ok={set_ok}, read_back={applied:?}); leaving voltage unchanged (no \
                         undervolt: clock not confirmed lowered, old freq stays paired with old \
                         voltage)",
                        cluster.name()
                    );
                }
                false
            }
        };
        confirmed.then_some(()).ok_or(())
    })
    .is_ok()
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
