#[cfg(feature = "rockchip-dwmmc")]
use alloc::format;

use log::info;
#[cfg(feature = "rockchip-dwmmc")]
use rdrive::probe::fdt::ClockLine;
use rdrive::{
    probe::{OnProbeError, fdt::ClockRef},
    register::FdtInfo,
};

#[cfg(feature = "rockchip-dwmmc")]
use crate::soc::scmi;

#[cfg(feature = "rockchip-dwmmc")]
fn scmi_clock_id(clock: &ClockRef) -> Option<u32> {
    (clock.cells > 0)
        .then(|| clock.specifier.first().copied())
        .flatten()
}

#[cfg(feature = "rockchip-dwmmc")]
fn is_scmi_clock_provider(info: &FdtInfo<'_>, clock: &ClockRef) -> bool {
    info.get_by_phandle(clock.phandle)
        .map(|node| {
            let node = node.as_node();
            node.name().starts_with("protocol@14")
                && node.get_property("reg").and_then(|prop| prop.get_u32()) == Some(0x14)
        })
        .unwrap_or(false)
}

#[cfg(feature = "rockchip-dwmmc")]
fn enable_scmi_clock(
    info: &FdtInfo<'_>,
    clock: &ClockRef,
    label: &str,
) -> Result<bool, OnProbeError> {
    if !is_scmi_clock_provider(info, clock) {
        return Ok(false);
    }
    let Some(clock_id) = scmi_clock_id(clock) else {
        return Ok(false);
    };
    scmi::enable_clock(clock.phandle, clock_id).ok_or_else(|| {
        OnProbeError::other(format!(
            "[{}] failed to enable SCMI {label} clock {:?} ({:#x})",
            info.node.name(),
            clock.name,
            clock_id
        ))
    })?;
    info!(
        "[{}] enabled {label} SCMI clock {:?} ({:#x})",
        info.node.name(),
        clock.name,
        clock_id
    );
    Ok(true)
}

#[cfg(not(feature = "rockchip-dwmmc"))]
fn enable_scmi_clock(
    _info: &FdtInfo<'_>,
    _clock: &ClockRef,
    _label: &str,
) -> Result<bool, OnProbeError> {
    Ok(false)
}

#[cfg(feature = "rockchip-dwmmc")]
pub(crate) fn rdrive_named_clock(
    info: &FdtInfo<'_>,
    name: &str,
) -> Result<Option<ClockLine>, OnProbeError> {
    let Some(clock) = info
        .clocks()?
        .into_iter()
        .find(|clock| clock.name.as_deref() == Some(name))
    else {
        return Ok(None);
    };
    if is_scmi_clock_provider(info, &clock) {
        return Ok(None);
    }
    info.clock_line(&clock).map(Some)
}

pub(crate) fn enable_node_clocks(info: &FdtInfo<'_>, label: &str) -> Result<(), OnProbeError> {
    for clock in info.clocks()? {
        if clock.select() == Some(0) {
            continue;
        }
        if enable_scmi_clock(info, &clock, label)? {
            continue;
        }

        let line = info.clock_line(&clock)?;
        line.enable()?;
        info!(
            "[{}] enabled {label} clock {:?} ({:#x})",
            info.node.name(),
            clock.name,
            line.id().raw()
        );
    }
    Ok(())
}

#[cfg(feature = "rockchip-dwmmc")]
pub(crate) fn scmi_named_clock(info: &FdtInfo<'_>, name: &str) -> Option<ScmiClockOps> {
    let clock = info.find_clk_by_name(name)?;
    let clock_id = scmi_clock_id(&clock)?;
    is_scmi_clock_provider(info, &clock).then_some(ScmiClockOps {
        phandle: clock.phandle,
        clock_id,
    })
}

#[cfg(feature = "rockchip-dwmmc")]
pub(crate) struct ScmiClockOps {
    phandle: fdt_edit::Phandle,
    clock_id: u32,
}

#[cfg(feature = "rockchip-dwmmc")]
impl ScmiClockOps {
    pub(crate) fn set_rate(&self, rate: u64) -> Option<()> {
        scmi::set_clock_rate(self.phandle, self.clock_id, rate)
    }

    pub(crate) fn rate(&self) -> Option<u64> {
        scmi::clock_rate(self.phandle, self.clock_id)
    }
}
