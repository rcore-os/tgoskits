//! RK3588 NPU resource preparation before exclusive guest passthrough.

use alloc::{format, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use fdt_edit::Phandle;
use log::info;
use rdrive::{
    DriverGeneric,
    probe::{OnProbeError, fdt::ClockRef},
    register::{FdtInfo, ProbeFdt},
};

use crate::soc::scmi;

const EXPECTED_CORE_REGIONS: [(u64, u64); 3] = [
    (0xfdab_0000, 0x1_0000),
    (0xfdac_0000, 0x1_0000),
    (0xfdad_0000, 0x1_0000),
];
const EXPECTED_POWER_DOMAINS: [(&str, u32); 3] = [("npu0", 9), ("npu1", 10), ("npu2", 11)];
const EXPECTED_CLOCKS: [(&str, u32); 8] = [
    ("clk_npu", 0x06),
    ("aclk0", 0x12d),
    ("aclk1", 0x122),
    ("aclk2", 0x124),
    ("hclk0", 0x12e),
    ("hclk1", 0x123),
    ("hclk2", 0x125),
    ("pclk", 0x131),
];
const EXPECTED_RESETS: [(&str, u32); 6] = [
    ("srst_a0", 0x1e6),
    ("srst_a1", 0x1b0),
    ("srst_a2", 0x1c0),
    ("srst_h0", 0x1e8),
    ("srst_h1", 0x1b2),
    ("srst_h2", 0x1c2),
];
const SCMI_NPU_CLOCK_ID: u32 = 6;
const SCMI_NPU_CLOCK_RATE_HZ: u64 = 200_000_000;

static HANDOFF_READY: AtomicBool = AtomicBool::new(false);

crate::model_register!(
    name: "RK3588 NPU guest handoff",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["rockchip,rk3588-rknpu"],
            on_probe: probe
        }
    ],
);

/// Require the host-side RK3588 NPU resource handoff to have completed.
///
/// AxVisor calls this before it creates any guest so a missing node or failed
/// power/clock/reset operation cannot degrade into a guest-visible timeout.
pub fn require_rk3588_npu_handoff() {
    assert!(
        HANDOFF_READY.load(Ordering::Acquire),
        "RK3588 NPU resources were not prepared for exclusive guest handoff"
    );
    info!("AXVISOR_RK3588_NPU_HANDOFF_REQUIRED ready=true");
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform_device) = probe.into_parts();
    validate_resource_contract(&info)?;
    let assigned_clock = assigned_scmi_clock(&info)?;

    scmi::set_clock_rate(
        assigned_clock.phandle,
        SCMI_NPU_CLOCK_ID,
        SCMI_NPU_CLOCK_RATE_HZ,
    )
    .ok_or_else(|| contract_error(&info, "failed to set the assigned SCMI NPU clock rate"))?;

    for domain in info.power_domain_lines()? {
        domain.power_on()?;
    }
    for clock in info.clocks()? {
        enable_clock(&info, &clock)?;
    }
    for reset in info.reset_lines()? {
        reset.deassert()?;
    }

    let observed_rate = scmi::clock_rate(assigned_clock.phandle, SCMI_NPU_CLOCK_ID)
        .ok_or_else(|| contract_error(&info, "failed to read back the SCMI NPU clock rate"))?;
    if observed_rate != SCMI_NPU_CLOCK_RATE_HZ {
        return Err(contract_error(
            &info,
            &format!(
                "SCMI NPU clock readback is {observed_rate} Hz, expected {SCMI_NPU_CLOCK_RATE_HZ} \
                 Hz"
            ),
        ));
    }

    platform_device.register(Rk3588NpuHandoff);
    HANDOFF_READY.store(true, Ordering::Release);
    info!(
        "AXVISOR_RK3588_NPU_HANDOFF_READY cores=3 power_domains=3 clocks=8 resets=6 \
         scmi_clock_id={} scmi_rate_hz={} host_submit=false",
        SCMI_NPU_CLOCK_ID, SCMI_NPU_CLOCK_RATE_HZ
    );
    Ok(())
}

fn validate_resource_contract(info: &FdtInfo<'_>) -> Result<(), OnProbeError> {
    let core_regions = info
        .node
        .regs()
        .into_iter()
        .map(|reg| (reg.address, reg.size.unwrap_or_default()))
        .collect::<Vec<_>>();
    if core_regions != EXPECTED_CORE_REGIONS {
        return Err(contract_error(
            info,
            &format!("unexpected NPU core regions: {core_regions:#x?}"),
        ));
    }

    let power_domains = info.power_domains()?;
    let power_domain_selectors = power_domains
        .iter()
        .map(|domain| (domain.name.as_deref(), domain.select()))
        .collect::<Vec<_>>();
    validate_named_selectors(
        info,
        "power domains",
        &power_domain_selectors,
        &EXPECTED_POWER_DOMAINS,
    )?;

    let clocks = info.clocks()?;
    let clock_selectors = clocks
        .iter()
        .map(|clock| (clock.name.as_deref(), clock.select()))
        .collect::<Vec<_>>();
    validate_named_selectors(info, "clocks", &clock_selectors, &EXPECTED_CLOCKS)?;

    let resets = info.resets()?;
    let reset_selectors = resets
        .iter()
        .map(|reset| (reset.name.as_deref(), reset.select()))
        .collect::<Vec<_>>();
    validate_named_selectors(info, "resets", &reset_selectors, &EXPECTED_RESETS)
}

fn validate_named_selectors(
    info: &FdtInfo<'_>,
    kind: &str,
    actual: &[(Option<&str>, Option<u32>)],
    expected: &[(&str, u32)],
) -> Result<(), OnProbeError> {
    if named_selectors_match(actual, expected) {
        Ok(())
    } else {
        Err(contract_error(
            info,
            &format!("unexpected {kind}: {actual:?}"),
        ))
    }
}

fn named_selectors_match(actual: &[(Option<&str>, Option<u32>)], expected: &[(&str, u32)]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(
            |((actual_name, actual_selector), (expected_name, expected_selector))| {
                *actual_name == Some(*expected_name) && *actual_selector == Some(*expected_selector)
            },
        )
}

struct AssignedScmiClock {
    phandle: Phandle,
}

fn assigned_scmi_clock(info: &FdtInfo<'_>) -> Result<AssignedScmiClock, OnProbeError> {
    let clock = info
        .find_clk_by_name("clk_npu")
        .ok_or_else(|| contract_error(info, "missing clk_npu"))?;
    if !is_scmi_clock_provider(info, &clock) || clock.select() != Some(SCMI_NPU_CLOCK_ID) {
        return Err(contract_error(info, "clk_npu is not SCMI clock 6"));
    }

    let assigned = info
        .node
        .as_node()
        .get_property("assigned-clocks")
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let assigned_rate = info
        .node
        .as_node()
        .get_property("assigned-clock-rates")
        .map(|property| property.get_u32_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let assigned_matches = assigned.len() == 2
        && Phandle::from(assigned[0]) == clock.phandle
        && assigned[1] == SCMI_NPU_CLOCK_ID;
    if !assigned_matches || assigned_rate.as_slice() != [SCMI_NPU_CLOCK_RATE_HZ as u32] {
        return Err(contract_error(
            info,
            &format!(
                "unexpected assigned clock contract: clocks={assigned:#x?}, \
                 rates={assigned_rate:?}"
            ),
        ));
    }

    Ok(AssignedScmiClock {
        phandle: clock.phandle,
    })
}

fn enable_clock(info: &FdtInfo<'_>, clock: &ClockRef) -> Result<(), OnProbeError> {
    if is_scmi_clock_provider(info, clock) {
        let clock_id = clock
            .select()
            .ok_or_else(|| contract_error(info, "SCMI clock has no selector"))?;
        return scmi::enable_clock(clock.phandle, clock_id)
            .ok_or_else(|| contract_error(info, "failed to enable the SCMI NPU clock"));
    }

    info.clock_line(clock)?.enable()
}

fn is_scmi_clock_provider(info: &FdtInfo<'_>, clock: &ClockRef) -> bool {
    info.get_by_phandle(clock.phandle)
        .map(|node| {
            let node = node.as_node();
            node.name().starts_with("protocol@14")
                && node
                    .get_property("reg")
                    .and_then(|property| property.get_u32())
                    == Some(0x14)
        })
        .unwrap_or(false)
}

fn contract_error(info: &FdtInfo<'_>, detail: &str) -> OnProbeError {
    OnProbeError::other(format!(
        "[{}] RK3588 NPU handoff contract error: {detail}",
        info.node.name()
    ))
}

struct Rk3588NpuHandoff;

impl DriverGeneric for Rk3588NpuHandoff {
    fn name(&self) -> &str {
        "rk3588-npu-guest-handoff"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_selector_contract_requires_exact_order_and_values() {
        let expected = [("first", 1), ("second", 2)];
        let valid = [(Some("first"), Some(1)), (Some("second"), Some(2))];
        let reordered = [(Some("second"), Some(2)), (Some("first"), Some(1))];

        assert!(named_selectors_match(&valid, &expected));
        assert!(!named_selectors_match(&reordered, &expected));
    }
}
