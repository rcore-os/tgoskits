//! OTP, PVTM, firmware and device-tree selection of confirmed OPPs.

use super::*;
use crate::KError;

/// BSP `OPP_LENGTH_LOW`: firmware interprets the low rate bits as a command.
const PVTPLL_LENGTH_LOW: u64 = 1 << 2;
#[cfg(target_arch = "aarch64")]
const SIP_PVTPLL_CFG: u32 = 0x8200_0029;
const PVTPLL_GET_INFO: u32 = 0;
const PVTPLL_LOW_TEMP: u32 = 2;

#[cfg(target_arch = "aarch64")]
fn pvtpll_firmware_call(subcommand: u32, clock_id: u32, value: u32) -> [u32; 8] {
    smccc::smc32(SIP_PVTPLL_CFG, [subcommand, clock_id, value, 0, 0, 0, 0])
}

#[cfg(not(target_arch = "aarch64"))]
fn pvtpll_firmware_call(_subcommand: u32, _clock_id: u32, _value: u32) -> [u32; 8] {
    [u32::MAX; 8]
}

struct PvtpllFirmware {
    low_temp: bool,
    legacy: bool,
}

fn pvtpll_firmware_info(clock_id: u32) -> Result<PvtpllFirmware, FrequencyError> {
    let result = pvtpll_firmware_call(PVTPLL_GET_INFO, clock_id, 0);
    if result[0] == u32::MAX {
        info!("cpufreq: PVTPLL clock id {clock_id} uses legacy firmware without SIP PVTPLL info");
        return Ok(PvtpllFirmware {
            low_temp: false,
            legacy: true,
        });
    }
    if result[0] != 0 {
        warn!(
            "cpufreq: PVTPLL clock id {clock_id} firmware info failed: {:#x}",
            result[0]
        );
        return Err(FrequencyError::NotReady);
    }
    Ok(PvtpllFirmware {
        low_temp: result[1] == 0,
        legacy: false,
    })
}

pub(super) fn configure_pvtpll_low_temp(clock_id: u32, low: bool) -> bool {
    let result = pvtpll_firmware_call(PVTPLL_LOW_TEMP, clock_id, u32::from(low));
    if result[0] != 0 {
        warn!(
            "cpufreq: PVTPLL clock id {clock_id} low-temperature mode {low} failed: {:#x}",
            result[0]
        );
    }
    result[0] == 0
}

fn configure_pvtpll_length(
    phandle: Phandle,
    clock_id: u32,
    boot_hz: u64,
    grade: u8,
    low_length_grade: Option<u32>,
    legacy_firmware: bool,
) -> bool {
    let Some(limit) = low_length_grade else {
        return true;
    };
    if u32::from(grade) > limit {
        return true;
    }
    // The BSP sends the low-length flag in the rate's low bits, then restores
    // the plain rate. A successful SCMI status and restored rate are both
    // required before this domain can advertise the selected OPPs.
    let requested = boot_hz | PVTPLL_LENGTH_LOW;
    let request = scmi::set_clock_rate_checked(phandle, clock_id, requested);
    let restored = set_and_verify(phandle, clock_id, boot_hz, boot_hz);
    if matches!(&request, Err(KError::InvalidArg { name: "rate" })) && legacy_firmware && restored {
        // The board's BL31 returns SMC_UNKNOWN for GET_INFO and rejects this
        // flagged SCMI rate. The BSP also keeps the ordinary OPPs in this
        // case. Their delivered frequencies require a separate board check.
        warn!(
            "cpufreq: PVTPLL clock id {clock_id} legacy firmware rejected low-length extension; \
             using confirmed plain rate"
        );
        return true;
    }
    if request.is_err() || !restored {
        warn!(
            "cpufreq: PVTPLL clock id {clock_id} low-length request or plain rate restore failed"
        );
    }
    request.is_ok() && restored
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
    if table_node
        .get_property("rockchip,pvtm-thermal-zone")
        .and_then(|property| property.as_str())
        != Some("soc-thermal")
    {
        return Err(FrequencyError::NotReady);
    }

    let serial = sensors::sku_serial().map_err(|_| FrequencyError::NotReady)?;
    let bin = HardwareSelection::bin_from_serial(serial);
    if bin != 0 {
        warn!(
            "cpufreq: {} J/M SKU bin {bin} is not validated; disabling CPUFreq selection",
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
    let opp_info = sensors::read_otp_bytes(
        usize::try_from(cell_reg.address).map_err(|_| FrequencyError::NotReady)?,
        usize::try_from(cell_reg.size.ok_or(FrequencyError::NotReady)?)
            .map_err(|_| FrequencyError::NotReady)?,
    )
    .map_err(|_| FrequencyError::NotReady)?;

    let temperature =
        sensors::soc_temperature_millidegrees().map_err(|_| FrequencyError::NotReady)?;
    let firmware = pvtpll_firmware_info(cluster.clock_id())?;
    if firmware.low_temp && !configure_pvtpll_low_temp(cluster.clock_id(), temperature < 10_000) {
        return Err(FrequencyError::HardwareFailure);
    }
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
    let margin_threshold_hz = u64::from(property_u32("intermediate-threshold-freq")?)
        .checked_mul(1000)
        .ok_or(FrequencyError::NotReady)?;
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
        || margin_threshold_hz != A55_MAX_HZ
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
    // The BSP changes read margin below the DT intermediate threshold. The
    // A76 bootstrap rate is above that threshold; lower it first while its
    // 750 mV supply remains confirmed, then restore before PVTM sampling.
    if boot_hz > margin_threshold_hz
        && !set_and_verify(phandle, cluster.clock_id(), margin_threshold_hz, boot_hz)
    {
        DOMAIN_READY[cluster.index()].store(false, Ordering::Release);
        return Err(FrequencyError::HardwareFailure);
    }
    let margin_established = margin.establish_for_voltage(measurement_uv).is_ok();
    let boot_restored = boot_hz <= margin_threshold_hz
        || set_and_verify(phandle, cluster.clock_id(), boot_hz, boot_hz);
    if !margin_established || !boot_restored {
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
    let sample = pvtm::read_raw_sample(grf.address, grf.size, offset);
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
    let corrected = pvtm::temperature_correct(
        sample,
        temperature,
        property_u32("rockchip,pvtm-ref-temp")? as i32,
        [*below as i32, *above as i32],
    )
    .ok_or(FrequencyError::NotReady)?;
    // Non-standard J/M bins fail closed above until their board voltage and
    // frequency measurements are validated. Keep the hardware-bin table out
    // of this path so the implementation cannot imply that those bins are
    // supported merely because a DTB contains `-hw` data.
    let grade_property = "rockchip,pvtm-voltage-sel";
    let grade_cells = table_node
        .get_property(grade_property)
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .ok_or(FrequencyError::NotReady)?;
    let (rows, remainder) = grade_cells.as_chunks::<3>();
    if !remainder.is_empty() {
        return Err(FrequencyError::NotReady);
    }
    let grade =
        u8::try_from(pvtm::select_voltage_grade(corrected, rows).ok_or(FrequencyError::NotReady)?)
            .map_err(|_| FrequencyError::NotReady)?;
    let Some(verified_maximum_hz) =
        super::board::verified_maximum_hz(matches!(cluster, Cluster::A55), grade)
    else {
        warn!(
            "cpufreq: {} PVTM grade {grade} has no board-validated high OPP; retaining boot OPP",
            cluster.name()
        );
        return Err(FrequencyError::NotReady);
    };
    let low_length_grade = table_node
        .get_property("rockchip,pvtm-low-len-sel")
        .and_then(|property| property.get_u32());
    if !matches!(cluster, Cluster::A55) && low_length_grade != Some(3) {
        return Err(FrequencyError::NotReady);
    }
    if !configure_pvtpll_length(
        phandle,
        cluster.clock_id(),
        boot_hz,
        grade,
        low_length_grade,
        firmware.legacy,
    ) {
        DOMAIN_READY[cluster.index()].store(false, Ordering::Release);
        return Err(FrequencyError::HardwareFailure);
    }
    let selection = HardwareSelection::from_otp(serial, grade, &opp_info)
        .map_err(|_| FrequencyError::NotReady)?;
    let mut selected = cpufreq_opp::parse_domain_opps(fdt, cpu.as_node(), selection)
        .map_err(|_| FrequencyError::NotReady)?;
    selected.retain(|opp| opp.frequency_hz <= verified_maximum_hz);
    if selected.is_empty() {
        return Err(FrequencyError::NotReady);
    }
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
    Ok(SelectedDomain {
        opps,
        margin,
        low_temp_firmware: firmware.low_temp,
    })
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
    // restoring A55 below the 1.608 GHz ring's 1.2 GHz DSU requirement.
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
    let selected = [little, big0, big1];
    for cluster in [Cluster::Big0, Cluster::Big1] {
        if selected[cluster.index()].is_some() {
            continue;
        }
        // No PVTM grade or margin was established for this cluster. At 750 mV
        // the 1.2 GHz PVTPLL ring is not a verified delivered-frequency OPP on
        // every board. The original 816 MHz ring is within the common boot
        // voltage envelope and needs no unverified rail or GRF write.
        if !set_and_verify(phandle, cluster.clock_id(), 816_000_000, A76_MAX_HZ) {
            warn!(
                "cpufreq: {} safe fallback clock could not be confirmed at 816 MHz",
                cluster.name()
            );
            disable_all_domains();
            return;
        }
        IDX[cluster.index()].store(0, Ordering::Release);
        FLOOR_IDX[cluster.index()].store(0, Ordering::Release);
        LIMITED_FALLBACK[cluster.index()].store(true, Ordering::Release);
        warn!(
            "cpufreq: {} OPP selection unavailable; limiting to 816 MHz at 750 mV",
            cluster.name()
        );
    }
    if !set_and_verify(phandle, A55_CLK_ID, A55_MAX_HZ, A55_PVTM_HZ) {
        disable_all_domains();
        return;
    }
    if DOMAIN_READY
        .iter()
        .any(|ready| !ready.load(Ordering::Acquire))
    {
        disable_all_domains();
        return;
    }
    if selected[Cluster::A55.index()].is_none() {
        // Set both unselected big rings to their original low rate before
        // closing the interface. At 750 mV the A55 bootstrap ring can deliver
        // above its nominal 1.008 GHz and is not a calibrated OPP either.
        disable_all_domains();
        return;
    }
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
