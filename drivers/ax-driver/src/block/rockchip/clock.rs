use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use fdt_edit::Node;
use log::{info, warn};
use rdif_clk::ClockId;
use rdrive::{
    Device,
    probe::{OnProbeError, fdt::ClockRef},
    register::FdtInfo,
};

#[cfg(feature = "rockchip-dwmmc")]
use crate::soc::scmi;

pub(crate) struct RockchipClockOps {
    node_name: String,
    clock_name: Option<String>,
    device: Device<rdif_clk::Clk>,
    id: ClockId,
}

impl RockchipClockOps {
    pub(crate) fn from_node_clock(
        info: &FdtInfo<'_>,
        clock: &ClockRef,
    ) -> Result<Option<Self>, OnProbeError> {
        let Some(clock_id) = clock.select() else {
            return Ok(None);
        };
        let Some(device_id) = info.phandle_to_device_id(clock.phandle) else {
            warn!(
                "[{}] clock {:?} phandle {} has no device id",
                info.node.name(),
                clock.name,
                clock.phandle
            );
            return Ok(None);
        };
        let device = rdrive::get::<rdif_clk::Clk>(device_id).map_err(|_| {
            OnProbeError::other(format!(
                "[{}] clock {:?} device {:?} is not registered",
                info.node.name(),
                clock.name,
                device_id
            ))
        })?;
        Ok(Some(Self {
            node_name: info.node.name().to_string(),
            clock_name: clock.name.clone(),
            device,
            id: ClockId::from(clock_id as usize),
        }))
    }

    pub(crate) fn named(info: &FdtInfo<'_>, name: &str) -> Result<Option<Self>, OnProbeError> {
        let Some(clock) = info.find_clk_by_name(name) else {
            return Ok(None);
        };
        Self::from_node_clock(info, &clock)
    }

    pub(crate) fn enable(&self) -> Result<(), OnProbeError> {
        let mut clock = self.lock()?;
        clock.enable(self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to enable clock {:?} ({:?}): {err:?}",
                self.node_name, self.clock_name, self.id
            ))
        })?;
        Ok(())
    }

    pub(crate) fn set_rate(&self, rate: u64) -> Result<(), OnProbeError> {
        let mut clock = self.lock()?;
        clock.set_rate(self.id, rate).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to set clock {:?} ({:?}) to {} Hz: {err:?}",
                self.node_name, self.clock_name, self.id, rate
            ))
        })?;
        Ok(())
    }

    pub(crate) fn rate(&self) -> Result<u64, OnProbeError> {
        let clock = self.lock()?;
        clock.get_rate(self.id).map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to read clock {:?} ({:?}): {err:?}",
                self.node_name, self.clock_name, self.id
            ))
        })
    }

    pub(crate) fn id(&self) -> ClockId {
        self.id
    }

    fn lock(&self) -> Result<rdrive::DeviceGuard<rdif_clk::Clk>, OnProbeError> {
        self.device.lock().map_err(|err| {
            OnProbeError::other(format!(
                "[{}] failed to lock clock {:?} ({:?}): {err}",
                self.node_name, self.clock_name, self.id
            ))
        })
    }
}

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

pub(crate) fn enable_node_clocks(info: &FdtInfo<'_>, label: &str) {
    for clock in info.node.clocks() {
        if clock.select().unwrap_or(0) == 0 {
            continue;
        }
        match enable_scmi_clock(info, &clock, label) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(err) => {
                warn!(
                    "[{}] {label} clock {:?} enable skipped: {err}",
                    info.node.name(),
                    clock.name
                );
                continue;
            }
        }
        match RockchipClockOps::from_node_clock(info, &clock).and_then(|clock| {
            let Some(clock) = clock else {
                return Ok(None);
            };
            clock.enable()?;
            Ok(Some(clock.id()))
        }) {
            Ok(Some(clock_id)) => info!(
                "[{}] enabled {label} clock {:?} ({:?})",
                info.node.name(),
                clock.name,
                clock_id
            ),
            Ok(None) => {}
            Err(err) => warn!(
                "[{}] {label} clock {:?} enable skipped: {err}",
                info.node.name(),
                clock.name
            ),
        }
    }
}

pub(crate) fn apply_assigned_clocks(info: &FdtInfo<'_>, label: &str) -> Result<(), OnProbeError> {
    let clocks = parse_clock_cells(info.node.as_node(), "assigned-clocks")?;
    let rates = info
        .node
        .as_node()
        .get_property("assigned-clock-rates")
        .map(|prop| prop.get_u32_iter().map(u64::from).collect::<Vec<_>>())
        .unwrap_or_default();
    for (index, (phandle, clock_id)) in clocks.into_iter().enumerate() {
        let Some(rate) = rates.get(index).copied() else {
            continue;
        };
        if rate == 0 {
            continue;
        }
        let Some(device_id) = info.phandle_to_device_id(phandle.into()) else {
            warn!(
                "[{}] assigned {label} clock phandle {} has no device id",
                info.node.name(),
                phandle
            );
            continue;
        };
        let device = rdrive::get::<rdif_clk::Clk>(device_id).map_err(|_| {
            OnProbeError::other(format!(
                "[{}] assigned {label} clock device {:?} is not registered",
                info.node.name(),
                device_id
            ))
        })?;
        let ops = RockchipClockOps {
            node_name: info.node.name().to_string(),
            clock_name: None,
            device,
            id: ClockId::from(clock_id as usize),
        };
        ops.set_rate(rate)?;
        info!(
            "[{}] assigned {label} clock {:?} to {} Hz",
            info.node.name(),
            ops.id(),
            rate
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

fn parse_clock_cells(node: &Node, property: &str) -> Result<Vec<(u32, u32)>, OnProbeError> {
    let Some(prop) = node.get_property(property) else {
        return Ok(Vec::new());
    };
    let cells = prop.get_u32_iter().collect::<Vec<_>>();
    if cells.len() % 2 != 0 {
        return Err(OnProbeError::other(format!(
            "[{}] has malformed {property}",
            node.name()
        )));
    }
    Ok(cells.chunks(2).map(|chunk| (chunk[0], chunk[1])).collect())
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn parse_clock_cells_reads_phandle_and_clock_id_pairs() {
        let mut node = Node::new("mmc@fe2e0000");
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x1000_u32.to_be_bytes());
        raw.extend_from_slice(&314_u32.to_be_bytes());
        raw.extend_from_slice(&0x1000_u32.to_be_bytes());
        raw.extend_from_slice(&315_u32.to_be_bytes());
        node.add_property(fdt_edit::Property::new("assigned-clocks", raw));

        assert_eq!(
            parse_clock_cells(&node, "assigned-clocks").unwrap(),
            vec![(0x1000, 314), (0x1000, 315)]
        );
    }

    #[test]
    fn parse_clock_cells_rejects_malformed_cells() {
        let mut node = Node::new("mmc@fe2e0000");
        node.add_property(fdt_edit::Property::new(
            "assigned-clocks",
            0x1000_u32.to_be_bytes().to_vec(),
        ));

        assert!(parse_clock_cells(&node, "assigned-clocks").is_err());
    }
}
