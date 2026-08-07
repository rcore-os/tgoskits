use log::info;
use rdrive::{probe::OnProbeError, register::FdtInfo};

/// Enables every non-placeholder clock referenced by the RK3588 DWCMSHC node.
pub(crate) fn enable_node_clocks(info: &FdtInfo<'_>, label: &str) -> Result<(), OnProbeError> {
    for clock in info.clocks()? {
        if clock.select() == Some(0) {
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
