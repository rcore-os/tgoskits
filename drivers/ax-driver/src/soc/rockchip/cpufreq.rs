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

//! RK3588 CPU DVFS: SCMI clocks + PMIC rail-voltage alignment + ondemand governor.
//!
//! The RK3588 CPU clock is voltage-coupled — an SCMI clock id selects a PVTPLL
//! ring whose *delivered* frequency tracks the core rail voltage — and the SCMI
//! interface is frequency-only. Exact DVFS therefore needs BOTH levers together:
//! the SCMI ring *and* the matching rail voltage. This driver does three things,
//! in order:
//!
//! 1. **Set each cluster clock** to its boot target over the board-proven SCMI
//!    seam ([`set_and_verify`]). The three CPU domains and their SCMI clock ids
//!    (ground truth `orangepi5plus.dts`: `cpu@0..300` → `<scmi 0>`, `cpu@400/500`
//!    → `<scmi 2>`, `cpu@600/700` → `<scmi 3>`):
//!
//!    | cluster        | SCMI clock id | boot target (MHz) |
//!    |----------------|---------------|-------------------|
//!    | A55 (little)   | 0             | 1008              |
//!    | A76 big pair 0 | 2             | 1200              |
//!    | A76 big pair 1 | 3             | 1200              |
//!
//! 2. **Align each rail voltage to the OPP** ([`align_rail_voltages_to_opp`]).
//!    Boot firmware leaves the rails high (~800 mV), so the coupled clock
//!    overshoots the SCMI target until each rail is lowered to its OPP-nominal
//!    (0.675 V for these OPPs on the standard SKU). Lowering cannot undervolt: the
//!    coupled clock tracks the rail down in lockstep. NOTE the industrial
//!    RK3588J/M SKU (selected by the `specification_serial_number` nvmem cell)
//!    puts these OPPs at 0.75 V; the PMIC modules floor at 0.675 V, so gate the
//!    nominal on the SKU cell before running this on a J/M part.
//!
//! 3. **Hand off to the ondemand governor** ([`governor_poll`]). Once both
//!    CPU-rail PMIC buses are up, a dynamic governor scales each cluster's OPP to
//!    match load (see the governor section at the end of this file).
//!
//! Registration and ordering: a `PostKernel` / `DEFAULT` rdrive probe, so the CRU +
//! SCMI providers (registered at `CLK` priority) are already live, and it runs
//! inside `devices::probe_all_devices()` — **before** `start_secondary_cpus()` —
//! so the A76 clusters are reclocked while no core is scheduled on them (the live
//! A55 id-0 switch is BL31's glitch-free path). It binds to the CPU nodes rather
//! than `arm,scmi-smc` (which the SCMI driver already owns; a second driver on
//! that node would never get an `on_probe`), and applies exactly once via a
//! one-shot guard because several `cpu@*` nodes match.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use fdt_edit::Phandle;
use log::{info, warn};

use crate::{probe::OnProbeError, register::ProbeFdt, soc::scmi};

/// SCMI clock id of the A55 (little) cluster — cpu0..3.
const A55_CLK_ID: u32 = 0;
/// SCMI clock ids of the two A76 (big) cluster pairs — cpu4/5 and cpu6/7. Both
/// must be set; they cover different core pairs.
const A76_CLK_IDS: [u32; 2] = [2, 3];

/// A55 target and hard ceiling: the top OPP still on the 816 MHz boot voltage
/// row. `set_clock_rate` must never be driven above this for the A55 cluster.
const A55_MAX_HZ: u64 = 1_008_000_000;
/// A76 target and hard ceiling: the top OPP still on the 816 MHz boot voltage
/// row. `set_clock_rate` must never be driven above this for an A76 cluster.
const A76_MAX_HZ: u64 = 1_200_000_000;

/// One-shot guard: several `cpu@*` nodes match, but the reclock runs once.
static APPLIED: AtomicBool = AtomicBool::new(false);

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

fn probe(_probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    // Several CPU nodes match this driver; only the first invocation reclocks.
    if APPLIED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    // The `scmi::*` helpers ignore the phandle (single global agent); pass a
    // dummy so we do not depend on parsing the node's clock specifier.
    let phandle = Phandle::from(0u32);

    // Safety preflight (read-only): confirm this firmware actually services the
    // CPU-cluster clocks before touching any of them. `describe_rates` changes
    // no state, so this cannot hang or perturb the clocks; treat its result as
    // accept/reject only. If any target id is rejected, leave every cluster at
    // its boot rate and bail — there is no raw-CRU fallback here.
    for id in [A55_CLK_ID, A76_CLK_IDS[0], A76_CLK_IDS[1]] {
        if scmi::describe_rates(phandle, id).is_none() {
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

    // Hand off to the dynamic ondemand governor. It cannot
    // live entirely in this crate — ax-driver sits *below* ax-task/ax-hal in the
    // dependency graph (they pull ax-driver back in via axplat-dyn), so a
    // task-spawning loop here would be a cyclic dep. Instead this driver exposes
    // the pure policy+apply (`governor_poll`, see the governor section at the end
    // of this file) and the kernel drives it from a periodic sleepable task. The
    // voltage lever above armed `GOV_READY` iff both PMIC buses came up.

    Ok(())
}

/// Programs `clock_id` to `target`, but never above `ceiling` (the hard cap on
/// the boot voltage row), then verifies the platform actually applied it.
/// Returns `true` only when the read-back matches the request.
///
/// `target == ceiling` for the boot targets; the clamp is defense in depth so a future
/// edit can never push a cluster past its boot-voltage-safe ceiling. A rejected
/// set or a deviating read-back is reported and returns `false` so the caller
/// stops rather than leaving a partial/phantom state — the board stays on
/// whatever rate the firmware last confirmed.
fn set_and_verify(phandle: Phandle, clock_id: u32, target: u64, ceiling: u64) -> bool {
    if target > ceiling {
        warn!(
            "cpufreq: refusing to set clock id {clock_id} to {target} Hz (above boot-safe ceiling \
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
                "cpufreq: clock id {clock_id} read back {applied} Hz ABOVE boot-safe ceiling \
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

/// OPP-nominal core voltage for the boot targets on the **standard** SKU: the
/// 675 mV row shared by the 816/1008/1200 MHz OPPs (board-confirmed from the
/// `cluster*-opp-table` `opp-microvolt`). The A55 1008 OPP and both A76 1200 OPPs
/// all sit here.
///
/// NOTE: the industrial RK3588J/M SKU puts these OPPs at 750 mV (`opp-j-m-*`). The
/// PMIC modules floor at 675 mV, so a 675 mV target would *under*-volt a J/M part.
/// This board is the standard SKU; if this driver is ever run on a J/M board, gate
/// this constant on the `specification_serial_number` nvmem SKU cell first.
const A76_NOMINAL_UV: u32 = 675_000;
const A55_NOMINAL_UV: u32 = 675_000;

/// One-shot A55 MOSI/write diagnostic (user-authorized). The RK806 read path is
/// dead (returns a bogus 0x00), so we cannot validate an A55 write by read-back.
/// This force-writes DCDC2 to the bounded-safe A55 OPP nominal and relies on
/// cpuprobe observing whether the A55 frequency drops — the only way to learn if
/// MOSI/writes physically reach the RK806 when reads do not. The write is clamped
/// to [675 mV, 800 mV] (the A55 boot-safe row) inside the PMIC module.
const A55_FORCE_WRITE_TEST: bool = true;

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

    // --- Apply: stepped, down-only, read-back-verified lower to OPP nominal. ---
    if a76_ok {
        let b0 = pmic_i2c::set_uv_stepped(pmic_i2c::RK8602_BIG0_ADDR, A76_NOMINAL_UV);
        let b1 = pmic_i2c::set_uv_stepped(pmic_i2c::RK8603_BIG1_ADDR, A76_NOMINAL_UV);
        info!("cpufreq: A76 rails -> {A76_NOMINAL_UV} uV (big0 ok={b0}, big1 ok={b1})");
    }
    // A55 (spi2/RK806): only lower it if the current-voltage read is trustworthy.
    // `set_uv_stepped` reads the rail before stepping down, so we must never lower a
    // rail we cannot read. The spi2/RK806 read path is not up yet (it returns a bogus
    // 0x00 == 500 mV), so skip the A55 write until that bring-up completes rather than
    // act on a false reading. A real A55 boot voltage is in [675 mV, 950 mV] per the
    // RK806 DCDC2 range and the cluster0 OPP table.
    match if a55_ok { pmic_spi::get_uv() } else { None } {
        Some(v) if (675_000..=950_000).contains(&v) => {
            let a55 = pmic_spi::set_uv_stepped(A55_NOMINAL_UV);
            info!("cpufreq: A55 rail {v} -> {A55_NOMINAL_UV} uV (ok={a55})");
        }
        other => warn!(
            "cpufreq: A55 boot voltage not trustworthy ({other:?} uV); skipping A55 write until \
             spi2/RK806 bring-up completes (A76 unaffected)"
        ),
    }

    // A55 MOSI/write diagnostic: force-write DCDC2 to the bounded-safe nominal and
    // let cpuprobe reveal whether writes reach the RK806 even though reads don't.
    if A55_FORCE_WRITE_TEST && a55_ok {
        let ok = pmic_spi::force_write_dcdc2(A55_NOMINAL_UV);
        info!(
            "cpufreq: A55 MOSI/write test -> {A55_NOMINAL_UV} uV (write xfer_ok={ok}); watch A55 \
             cpuprobe (req=0..3) for a freq drop if the write reached"
        );
    }

    // Arm the dynamic governor only if both PMIC buses came up, so it never tries
    // to move a rail it cannot drive. If either failed, every cluster stays on the
    // boot OPP the SCMI reclock already set (safe: that is the fixed-boot state).
    if GOVERNOR_ENABLE && a76_ok && a55_ok {
        GOV_READY.store(true, Ordering::Release);
        info!("cpufreq: ondemand governor armed (both PMIC buses up)");
    } else if GOVERNOR_ENABLE {
        warn!(
            "cpufreq: ondemand governor NOT armed (a76_pmic={a76_ok}, a55_pmic={a55_ok}); \
             clusters stay on boot OPP"
        );
    }
}

// ===========================================================================
// Dynamic ondemand governor
// ===========================================================================
//
// The voltage lever above pins each cluster at a single boot OPP. This governor
// makes DVFS *dynamic*: it samples per-CPU busy time and moves each cluster's
// OPP up and down to track load, the way Linux's `ondemand`/`schedutil` do.
//
// Split by cost, exactly like Linux: the *accounting* is a cheap per-CPU counter
// bumped in the scheduler tick (`ax_task::cpu_busy_ticks`, fed by the non-idle
// branch of `scheduler_timer_tick`), but the *apply* is slow and SLEEPS — an
// SCMI clock set is an SMC into BL31, and the paired PMIC write is an I2C/SPI
// transaction with a millisecond voltage ramp. Neither may run in the tick
// handler, so the decision+apply live in a periodic sleepable kernel task. This
// is exactly why Linux's old ondemand used a deferred timer and schedutil kicks
// a kthread rather than reprogramming the OPP inline in the tick.

/// Master gate for the dynamic governor. When `false`, each cluster simply stays
/// on the fixed boot OPP the voltage alignment left it on.
const GOVERNOR_ENABLE: bool = true;

/// An operating performance point: the SCMI ring target to program, the rail
/// voltage to pair with it, and the frequency that combination actually delivers
/// (`mhz`, board-measured — see the calibration section). Because the PVTPLL is
/// voltage-coupled, the delivered `mhz` is generally NOT the `ring_khz`; the
/// governor reports `mhz`.
#[derive(Clone, Copy)]
struct Opp {
    ring_khz: u32,
    uv: u32,
    mhz: u32,
}

/// A76 (big) OPP ladder, low→high, from the on-board calibration sweep. The clock
/// is voltage-coupled, so this ladder is a HYBRID: below the 675 mV exact point it
/// scales the SCMI ring (408/816/1200 @ 675 mV land on target); above it, it holds
/// the ring at 1200 and raises the *voltage* — the delivered freq climbs while
/// staying over-volted (each rung's voltage exceeds the delivered freq's DT
/// nominal, so never an undervolt). Scaling the ring instead (e.g. ring 1608 @
/// 762.5 mV) over-delivers ~1733 MHz = ~125 mV of undervolt (measured), so it is
/// avoided. Top rung 1725 MHz @ 925 mV is the calibration sweep's safe maximum
/// (over-volted ~110 mV vs the delivered freq's DT nominal); board-validated
/// all-core (threads=8) with no PSU brownout.
const A76_OPPS: &[Opp] = &[
    Opp {
        ring_khz: 408_000,
        uv: 675_000,
        mhz: 408,
    },
    Opp {
        ring_khz: 816_000,
        uv: 675_000,
        mhz: 816,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 675_000,
        mhz: 1189,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 725_000,
        mhz: 1318,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 800_000,
        mhz: 1491,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 850_000,
        mhz: 1592,
    },
    Opp {
        ring_khz: 1_200_000,
        uv: 925_000,
        mhz: 1725,
    },
];

/// A55 (little) OPP ladder, low→high, same hybrid rationale: ring-scaled below the
/// 675 mV point, then ring 1008 with rising voltage. Top rung 1523 MHz @ 950 mV
/// (the RK806 force-write ceiling), the sweep's safe maximum for the little cluster.
const A55_OPPS: &[Opp] = &[
    Opp {
        ring_khz: 408_000,
        uv: 675_000,
        mhz: 408,
    },
    Opp {
        ring_khz: 816_000,
        uv: 675_000,
        mhz: 816,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 675_000,
        mhz: 1021,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 762_500,
        mhz: 1212,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 800_000,
        mhz: 1285,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 850_000,
        mhz: 1372,
    },
    Opp {
        ring_khz: 1_008_000,
        uv: 950_000,
        mhz: 1523,
    },
];

/// Index into each ladder of the boot OPP the voltage lever leaves the cluster on:
/// A55 1008 MHz and A76 1200 MHz are both element 2 (the 675 mV rung). The governor
/// starts tracking here so its first move is relative to the known boot state; from
/// idle it decays down the ring-scaled rungs and under load climbs the voltage ones.
const BOOT_OPP_IDX: usize = 2;

/// The three DVFS domains (one little cluster, two big pairs).
#[derive(Clone, Copy)]
enum Cluster {
    A55,
    Big0,
    Big1,
}

impl Cluster {
    fn opps(self) -> &'static [Opp] {
        match self {
            Cluster::A55 => A55_OPPS,
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

    /// CPUs whose busy time this domain aggregates (RK3588 topology: cpu0-3 A55,
    /// cpu4-5 big pair 0, cpu6-7 big pair 1).
    fn cpus(self) -> core::ops::Range<usize> {
        match self {
            Cluster::A55 => 0..4,
            Cluster::Big0 => 4..6,
            Cluster::Big1 => 6..8,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Cluster::A55 => "A55",
            Cluster::Big0 => "A76b0",
            Cluster::Big1 => "A76b1",
        }
    }

    /// Set this domain's rail to `uv`. A76 uses the read-back-verified I2C
    /// regulator; A55 uses the bounded force-write (its RK806 read is a
    /// scope-wall, but writes reach the chip — proven by the rail-alignment freq drop).
    /// Both PMIC helpers clamp to their rail's safe envelope internally.
    fn set_voltage(self, uv: u32) -> bool {
        use super::{pmic_i2c, pmic_spi};
        match self {
            Cluster::A55 => pmic_spi::force_write_dcdc2(uv),
            Cluster::Big0 => pmic_i2c::set_uv(pmic_i2c::RK8602_BIG0_ADDR, uv),
            Cluster::Big1 => pmic_i2c::set_uv(pmic_i2c::RK8603_BIG1_ADDR, uv),
        }
    }
}

/// Apply an OPP to a domain as a matched (voltage, frequency) pair, ordered so
/// the voltage-coupled clock never overshoots its rail:
///   - going UP:   raise voltage first, then the SCMI ring (clock follows up);
///   - going DOWN: lower the SCMI ring first, then voltage (clock follows down).
///
/// TRANSACTIONAL: stops at the first failed step instead of falling through to
/// the second (which could otherwise create exactly the freq-high/voltage-low
/// window this ordering exists to prevent), and returns `true` only when BOTH
/// steps are confirmed. The caller ([`governor_poll`]) must commit its software
/// OPP index only on `true`; on `false` it must keep tracking the last
/// confirmed index so the next poll retries from there.
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
    let phandle = Phandle::from(0u32);
    let hz = opp.ring_khz as u64 * 1_000;
    if going_up {
        if !cluster.set_voltage(opp.uv) {
            warn!(
                "cpufreq: {} upshift to {} mV failed; leaving clock id {} unchanged (no \
                 undervolt: old freq stays paired with old voltage)",
                cluster.name(),
                opp.uv / 1_000,
                cluster.clock_id()
            );
            return false;
        }
        if scmi::set_clock_rate(phandle, cluster.clock_id(), hz).is_none() {
            warn!(
                "cpufreq: {} voltage already raised to {} mV but SCMI rejected clock id {} -> {} \
                 Hz; not committing this OPP (safe: over-volted for the old, lower clock, never \
                 under-volted)",
                cluster.name(),
                opp.uv / 1_000,
                cluster.clock_id(),
                hz
            );
            return false;
        }
    } else {
        if scmi::set_clock_rate(phandle, cluster.clock_id(), hz).is_none() {
            warn!(
                "cpufreq: {} downshift: SCMI rejected clock id {} -> {} Hz; leaving voltage \
                 unchanged (no undervolt: old freq stays paired with old voltage)",
                cluster.name(),
                cluster.clock_id(),
                hz
            );
            return false;
        }
        if !cluster.set_voltage(opp.uv) {
            warn!(
                "cpufreq: {} clock already lowered to {} Hz but voltage write to {} mV failed; \
                 not committing this OPP (safe: over-volted for the new, lower clock, never \
                 under-volted)",
                cluster.name(),
                hz,
                opp.uv / 1_000
            );
            return false;
        }
    }
    true
}

/// Period, in ms, the kernel governor task sleeps between [`governor_poll`]s.
const GOV_PERIOD_MS: u64 = 100;
/// Busy% at or above which a domain jumps straight to its top OPP (ondemand's
/// signature fast attack: respond to a load spike in one step).
const UP_THRESHOLD_PCT: u64 = 80;
/// Busy% below which a domain steps down one OPP (slow decay: shed frequency
/// gradually so a brief idle dip does not collapse a still-busy workload).
const DOWN_THRESHOLD_PCT: u64 = 30;

/// Set once the voltage lever confirmed both PMIC buses are up. Until then the
/// governor must not move any rail (see `align_rail_voltages_to_opp`).
static GOV_READY: AtomicBool = AtomicBool::new(false);

/// Per-CPU busy-tick value from the previous poll (RK3588 has 8 cores). Only the
/// single governor task ever touches these, so `Relaxed` is sufficient.
static LAST_BUSY: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

/// Per-cluster current OPP index (A55, big0, big1), starting on the boot OPP the
/// voltage lever pinned.
static IDX: [AtomicUsize; 3] = [const { AtomicUsize::new(BOOT_OPP_IDX) }; 3];

/// Cleared until the first [`governor_poll`] has recorded a busy baseline. The
/// first call must only prime `LAST_BUSY`, not decide: its delta is measured
/// from zero and would otherwise fold in every busy tick accumulated since boot,
/// spuriously pegging every cluster to its top OPP for one window.
static PRIMED: AtomicBool = AtomicBool::new(false);

/// Whether the dynamic governor should run: enabled at compile time *and* armed
/// by the voltage lever (both PMIC buses up). The kernel checks this once before
/// spawning its periodic governor task, so a failed PMIC bring-up leaves every
/// cluster safely on the boot OPP instead of being scaled with no voltage lever.
pub fn governor_wanted() -> bool {
    GOVERNOR_ENABLE && GOV_READY.load(Ordering::Acquire)
}

/// Period, in milliseconds, the kernel governor task should sleep between calls
/// to [`governor_poll`].
pub fn governor_period_ms() -> u64 {
    GOV_PERIOD_MS
}

/// One ondemand iteration, called periodically by the kernel governor task with
/// a fresh snapshot of every CPU's cumulative busy-tick counter
/// (`ax_task::cpu_busy_ticks`). Scores each core's busy% over the last window and
/// moves each cluster's OPP from its busiest core: any saturated core jumps the
/// cluster to its top OPP, and the cluster sheds a step only when all its cores
/// are near-idle. Applies via SCMI+PMIC. Pure w.r.t. the task runtime — it
/// neither sleeps nor spawns — so this crate needs no dependency on
/// ax-task/ax-hal (which would be a cyclic dep through axplat-dyn).
///
/// `busy[i]` is CPU `i`'s counter; indices past the slice, or offline CPUs whose
/// counter never advances, simply read as idle — conservative (never over-scales).
pub fn governor_poll(busy: &[u64]) {
    if !governor_wanted() {
        return;
    }

    // The task sleeps `GOV_PERIOD_MS`, so ~`GOV_PERIOD_MS / 10` scheduler ticks
    // (10 ms/tick, TICKS_PER_SEC = 100) elapse per window. Using the nominal
    // window needs no clock here; a late wake only under-reports load (busy% is
    // clamped below), which is safe — it can never spuriously over-scale.
    const WINDOW_TICKS: u64 = if GOV_PERIOD_MS / 10 == 0 {
        1
    } else {
        GOV_PERIOD_MS / 10
    };

    // First call only establishes the baseline (see `PRIMED`); still walk every
    // cluster below so all `LAST_BUSY` entries are seeded, but make no OPP change.
    let priming = !PRIMED.swap(true, Ordering::Relaxed);

    for (ci, &cluster) in [Cluster::A55, Cluster::Big0, Cluster::Big1]
        .iter()
        .enumerate()
    {
        // Score each core in the cluster individually this window. A cluster
        // shares ONE clock, so a single saturated core is reason to raise the
        // whole cluster — this matches Linux schedutil/ondemand, which drive a
        // frequency domain from its busiest CPU. Averaging instead (an earlier
        // bug) buried one CPU-bound thread among its idle siblings: a single
        // thread on the 2-core A76 pair only reads 50%, below the up-threshold,
        // so the cluster never boosted. Each CPU belongs to exactly one cluster,
        // so every LAST_BUSY entry is refreshed exactly once per poll.
        let mut any_core_high = false; // some core wants the top OPP
        let mut all_cores_low = true; // every core is near-idle → shed one step
        let mut peak_pct = 0u64; // busiest core, for the log line
        let mut n = 0u64;
        for cpu in cluster.cpus() {
            let now = busy.get(cpu).copied().unwrap_or(0);
            let last = LAST_BUSY[cpu].swap(now, Ordering::Relaxed);
            // Per-core busy% = busy_ticks / window_ticks (one core), clamped.
            let pct = ((now.saturating_sub(last) * 100) / WINDOW_TICKS).min(100);
            if pct >= UP_THRESHOLD_PCT {
                any_core_high = true;
            }
            if pct >= DOWN_THRESHOLD_PCT {
                all_cores_low = false;
            }
            if pct > peak_pct {
                peak_pct = pct;
            }
            n += 1;
        }
        if n == 0 || priming {
            continue;
        }

        let opps = cluster.opps();
        let cur = IDX[ci].load(Ordering::Relaxed);
        let new = if any_core_high {
            opps.len() - 1 // fast attack: jump straight to the top OPP
        } else if all_cores_low && cur > 0 {
            cur - 1 // slow decay: shed one step only when the whole cluster is idle
        } else {
            cur
        };

        if new != cur {
            // Only commit IDX (the software record of the last CONFIRMED OPP)
            // when both the voltage write and the SCMI clock set are verified
            // successful. On failure, IDX is left at `cur` — the hardware is
            // always left in a safe (never-undervolted, see `apply_opp`) state
            // for that index, so the next poll retries the same climb/descent
            // from a known-good starting point rather than silently pretending
            // the change took effect.
            if apply_opp(cluster, opps[new], new > cur) {
                IDX[ci].store(new, Ordering::Relaxed);
                info!(
                    "gov: {} peak={}% opp {}->{} = {} MHz @ {} mV",
                    cluster.name(),
                    peak_pct,
                    cur,
                    new,
                    opps[new].mhz,
                    opps[new].uv / 1_000
                );
            } else {
                warn!(
                    "gov: {} peak={}% opp {}->{} FAILED to apply; staying at {} ({} MHz @ {} mV)",
                    cluster.name(),
                    peak_pct,
                    cur,
                    new,
                    cur,
                    opps[cur].mhz,
                    opps[cur].uv / 1_000
                );
            }
        }
    }
}

// ===========================================================================
// OPP calibration sweep (gated, one-shot)
// ===========================================================================
//
// The PVTPLL clock is voltage-coupled, so the delivered frequency for a given
// SCMI ring drifts with rail voltage, and the drift grows at the higher OPPs
// (ring=1608 @ 762.5 mV measured near ~1733 MHz on-board). To use the
// 1416/1608/1800 rungs SAFELY we must know the *actual* delivered frequency at
// each (ring, voltage); this sweep measures it directly via the PMU cycle
// counter. It runs once, gated by `CALIBRATE`, from early `init()` (before the
// console tty handoff) so its `CAL` log lines reach the serial console — the
// governor's own transition logs do not, because they fire post-handoff.

/// One-shot gate: when true, `init()` runs [`calibrate_cluster`] per cluster
/// (governor NOT spawned) and the board logs a `CAL` grid; leave false for
/// production. Requires the PMU cycle counter enabled at boot (axcpu `init_trap`).
const CALIBRATE: bool = false;

/// Sweep points `(rail_uV, ring_kHz)`. Round 1 showed the delivered frequency is
/// dominated by voltage and that at any DT (ring=F, V_nom(F)) pair the delivery
/// *over*-shoots F (undervolt). The safe lever is instead a FIXED low ring with a
/// rising voltage: the delivered freq climbs but stays over-volted. So round 2
/// maps ring 1200 across the full voltage range (plus two higher-ring cross-checks
/// to see where the ring stops mattering). Every point keeps V >= V_nom(ring), so
/// no measured point undervolts a live core; list is voltage-non-decreasing.
const CAL_A76: &[(u32, u32)] = &[
    (675_000, 1_200_000),
    (725_000, 1_200_000),
    (762_500, 1_200_000),
    (800_000, 1_200_000),
    (850_000, 1_200_000),
    (925_000, 1_200_000),
    (850_000, 1_416_000), // cross-check: does a higher ring beat ring 1200 at 850?
    (925_000, 1_608_000), // cross-check at 925
];

/// A55: fixed ring 1008 across the voltage range (RK806 force-write caps at 950 mV).
const CAL_A55: &[(u32, u32)] = &[
    (675_000, 1_008_000),
    (712_500, 1_008_000),
    (762_500, 1_008_000),
    (800_000, 1_008_000),
    (850_000, 1_008_000),
    (950_000, 1_008_000),
    (850_000, 1_416_000), // cross-check
    (950_000, 1_608_000), // cross-check
];

/// Whether to run the calibration sweep this boot (compile gate + PMIC armed).
pub fn calibrate_wanted() -> bool {
    CALIBRATE && GOV_READY.load(Ordering::Acquire)
}

// The calibration sweep is board-only (gated off by `CALIBRATE = false` in
// production), but `rk3588-cpufreq` is a public feature and must still COMPILE
// on a non-aarch64 host (e.g. a plain `cargo build`/`cargo test` on the CI
// host). These four leaf reads are the only AArch64 inline asm in this module;
// everything above them (`measure_mhz`, `cal_delay_ms`, `calibrate_cluster`)
// is arch-generic and calls only these, so gating just the leaves keeps the
// aarch64/board behavior byte-for-byte identical while giving every other
// target a harmless stub (0 never causes a hang: `cal_delay_ms`'s `frq.max(1)`
// and `measure_mhz`'s zero-length window both degenerate to an immediate
// return rather than spinning).
#[cfg(target_arch = "aarch64")]
#[inline]
fn rd_pmccntr() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, pmccntr_el0", out(reg) v) };
    v
}
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn rd_pmccntr() -> u64 {
    0
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn rd_cntvct() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v) };
    v
}
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn rd_cntvct() -> u64 {
    0
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn rd_cntfrq() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v) };
    v
}
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn rd_cntfrq() -> u64 {
    0
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn rd_mpidr() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) v) };
    v
}
#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn rd_mpidr() -> u64 {
    0
}

/// Busy-wait `ms` milliseconds against the fixed-rate `CNTVCT` clock.
fn cal_delay_ms(ms: u64) {
    let frq = rd_cntfrq().max(1);
    let start = rd_cntvct();
    let ticks = frq * ms / 1000;
    while rd_cntvct().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

/// Measure the CURRENT core's frequency (MHz) by counting CPU cycles
/// (`PMCCNTR_EL0`) over a fixed ~60 ms window timed by `CNTVCT_EL0`. The counter
/// is 32-bit (no `PMCR_EL0.LC`), which cannot wrap over this window.
fn measure_mhz() -> u32 {
    let frq = rd_cntfrq().max(1);
    let window = frq * 60 / 1000; // 60 ms in cntvct ticks
    let t0 = rd_cntvct();
    let c0 = rd_pmccntr() as u32;
    while rd_cntvct().wrapping_sub(t0) < window {
        core::hint::spin_loop();
    }
    let t1 = rd_cntvct();
    let c1 = rd_pmccntr() as u32;
    let dvct = t1.wrapping_sub(t0).max(1);
    let dcyc = c1.wrapping_sub(c0) as u64; // 32-bit wrap-safe
    ((dcyc * frq) / (dvct * 1_000_000)) as u32
}

/// Run the (voltage x ring) calibration sweep for one cluster ON THE CURRENT
/// CORE, logging the delivered frequency at each point. `cluster_idx`: 0=A55,
/// 1=A76 big0, 2=A76 big1. The caller must have pinned this task onto a core in
/// the target cluster (the logged `MPIDR` aff1 lets you confirm it). Restores a
/// safe boot OPP for the cluster on exit. PMIC access here is pure polling, so it
/// is safe on a non-boot core.
pub fn calibrate_cluster(cluster_idx: usize, intended_cpu: usize) {
    let (cluster, points, restore_khz) = match cluster_idx {
        0 => (Cluster::A55, CAL_A55, 1_008_000u32),
        1 => (Cluster::Big0, CAL_A76, 1_200_000u32),
        _ => (Cluster::Big1, CAL_A76, 1_200_000u32),
    };
    let mpidr = rd_mpidr();
    info!(
        "CAL begin cl={} cpu={} mpidr_aff1={} aff0={}",
        cluster.name(),
        intended_cpu,
        (mpidr >> 8) & 0xff,
        mpidr & 0xff
    );
    let phandle = Phandle::from(0u32);
    for &(uv, khz) in points {
        // Voltage first (points are voltage-non-decreasing, so this only ever
        // over-volts the current ring = safe), then the SCMI ring.
        let vok = cluster.set_voltage(uv);
        scmi::set_clock_rate(phandle, cluster.clock_id(), khz as u64 * 1_000);
        cal_delay_ms(25);
        let f = measure_mhz();
        info!(
            "CAL cl={} volt={}uV ring={}MHz volt_ok={} => {}MHz",
            cluster.name(),
            uv,
            khz / 1_000,
            vok,
            f
        );
    }
    // Restore: ring down first, then voltage down (safe order).
    scmi::set_clock_rate(phandle, cluster.clock_id(), restore_khz as u64 * 1_000);
    cluster.set_voltage(675_000);
    info!(
        "CAL end cl={} restored {}MHz@675mV",
        cluster.name(),
        restore_khz / 1_000
    );
}
