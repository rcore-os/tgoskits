#[cfg(feature = "rockchip-dwmmc")]
use alloc::format;
#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
use alloc::vec::Vec;

#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
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
#[derive(Clone, Copy)]
pub(crate) struct ScmiClockOps {
    phandle: fdt_edit::Phandle,
    clock_id: u32,
}

#[cfg(feature = "rockchip-dwmmc")]
impl ScmiClockOps {
    pub(crate) fn enable(&self) -> Option<()> {
        scmi::enable_clock(self.phandle, self.clock_id)
    }

    pub(crate) fn set_rate(&self, rate: u64) -> Option<()> {
        scmi::set_clock_rate(self.phandle, self.clock_id, rate)
    }

    pub(crate) fn rate(&self) -> Option<u64> {
        scmi::clock_rate(self.phandle, self.clock_id)
    }
}

/// A clock capability resolved during discovery but enabled only from a
/// controller initialization worker after its IRQ actions are live.
#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
pub(crate) enum StagedClockEnable {
    Rdrive(ClockLine),
    #[cfg(feature = "rockchip-dwmmc")]
    Scmi(ScmiClockOps),
}

#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
impl StagedClockEnable {
    pub(crate) fn enable(&self) -> Result<(), OnProbeError> {
        match self {
            Self::Rdrive(clock) => clock.enable(),
            #[cfg(feature = "rockchip-dwmmc")]
            Self::Scmi(clock) => clock.enable().ok_or_else(|| {
                OnProbeError::other(format!(
                    "failed to enable staged SCMI clock {:#x}",
                    clock.clock_id
                ))
            }),
        }
    }
}

/// Resolve the node's clock ownership without changing hardware state.
#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
pub(crate) fn staged_node_clocks(
    info: &FdtInfo<'_>,
) -> Result<Vec<StagedClockEnable>, OnProbeError> {
    info.clocks()?
        .into_iter()
        .filter(|clock| clock.select() != Some(0))
        .map(|clock| staged_clock_enable(info, clock))
        .collect()
}

#[cfg(any(feature = "rockchip-dwmmc", feature = "rockchip-sdhci"))]
fn staged_clock_enable(
    info: &FdtInfo<'_>,
    clock: ClockRef,
) -> Result<StagedClockEnable, OnProbeError> {
    #[cfg(feature = "rockchip-dwmmc")]
    {
        if is_scmi_clock_provider(info, &clock) {
            let clock_id = scmi_clock_id(&clock).ok_or_else(|| {
                OnProbeError::other(format!(
                    "[{}] staged SCMI clock {:?} has no selector",
                    info.node.name(),
                    clock.name
                ))
            })?;
            return Ok(StagedClockEnable::Scmi(ScmiClockOps {
                phandle: clock.phandle,
                clock_id,
            }));
        }
    }
    info.clock_line(&clock).map(StagedClockEnable::Rdrive)
}
