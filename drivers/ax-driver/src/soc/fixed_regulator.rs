extern crate alloc;

use alloc::{
    format,
    string::{String, ToString},
};
use core::time::Duration;

use fdt_edit::Node;
use log::{debug, info};
use rdif_pinctrl::{PinctrlDevice, PinctrlError};
use rdrive::{DriverGeneric, probe::OnProbeError, register::ProbeFdt};

const DRIVER_NAME: &str = "fixed-regulator";
const PINCTRL_OWNER: &str = "fixed-regulator";

crate::model_register!(
    name: DRIVER_NAME,
    level: ProbeLevel::PostKernel,
    priority: ProbePriority(64),
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["regulator-fixed"],
            on_probe: probe
        }
    ],
);

struct FixedRegulator {
    name: String,
}

impl DriverGeneric for FixedRegulator {
    fn name(&self) -> &str {
        &self.name
    }
}

fn probe(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let (info, platform) = probe.into_parts();
    let node = info.node.as_node();
    if !is_boot_enabled(node) {
        return Err(OnProbeError::NotMatch);
    }

    let name = regulator_name(node).unwrap_or_else(|| info.node.name().to_string());
    apply_fixed_regulator_pinctrl(node, &name)?;
    platform.register(FixedRegulator { name });
    Ok(())
}

fn apply_fixed_regulator_pinctrl(node: &Node, name: &str) -> Result<(), OnProbeError> {
    let Some(pinctrl) = rdrive::get_one::<PinctrlDevice>() else {
        debug!("Fixed regulator {name} has no PinctrlDevice; skip pinctrl enable");
        return Ok(());
    };
    let mut pinctrl = pinctrl
        .lock()
        .map_err(|err| OnProbeError::other(format!("failed to lock PinctrlDevice: {err}")))?;
    if pinctrl.fdt_parser().is_none() {
        debug!("Fixed regulator {name} has no FDT pinctrl parser; skip pinctrl enable");
        return Ok(());
    }

    let fdt = rdrive::with_fdt(Clone::clone)
        .ok_or_else(|| OnProbeError::other("live FDT not found for fixed regulator"))?;
    match pinctrl.apply_fdt_fixed_regulator(&fdt, node, PINCTRL_OWNER) {
        Ok(()) => {
            apply_startup_delay(node);
            info!("Fixed regulator {name} enabled via pinctrl");
            Ok(())
        }
        Err(PinctrlError::NotAvailable) => {
            debug!("Fixed regulator {name} has no pinctrl GPIO line; skip pinctrl enable");
            Ok(())
        }
        Err(err) => Err(OnProbeError::other(format!(
            "failed to enable fixed regulator {name} via pinctrl: {err}"
        ))),
    }
}

fn is_boot_enabled(node: &Node) -> bool {
    node.get_property("regulator-boot-on").is_some()
        || node.get_property("regulator-always-on").is_some()
}

fn regulator_name(node: &Node) -> Option<String> {
    node.get_property("regulator-name")
        .and_then(|prop| prop.as_str_iter().next())
        .map(ToString::to_string)
}

fn apply_startup_delay(node: &Node) {
    let startup_delay_us = node
        .get_property("startup-delay-us")
        .and_then(|prop| prop.get_u32())
        .unwrap_or(0);
    if startup_delay_us != 0 {
        axklib::time::busy_wait(Duration::from_micros(u64::from(startup_delay_us)));
    }
}
