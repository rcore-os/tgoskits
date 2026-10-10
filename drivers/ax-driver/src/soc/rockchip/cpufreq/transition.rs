//! Read-back-confirmed boot rail alignment and OPP transitions.

use super::*;

/// Programs `clock_id` to `target`, but never above the caller's confirmed
/// rail-voltage ceiling, then verifies the platform actually applied it.
/// Returns `true` only when the read-back matches the request.
///
/// The ceiling is the boot ring during probing and the PVTM measurement ring
/// after the 750 mV rail is confirmed. A rejected set or a deviating read-back
/// returns `false`; the caller then stops because the actual clock may differ
/// from the requested rate.
pub(super) fn set_and_verify(phandle: Phandle, clock_id: u32, target: u64, ceiling: u64) -> bool {
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
pub(super) fn read_mhz(phandle: Phandle, clock_id: u32) -> u64 {
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
pub(super) fn align_rail_voltages_to_opp() {
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
pub(super) fn apply_opp(cluster: Cluster, opp: Opp, going_up: bool) -> bool {
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
