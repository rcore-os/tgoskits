use ax_arm_pl031::Rtc;
use rdrive::{PlatformDevice, module_driver, probe::OnProbeError, register::FdtInfo};

use crate::drivers::iomap;

module_driver!(
    name: "pl031 rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl031"],
        on_probe: probe
    }],
);

fn probe(info: FdtInfo<'_>, _plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    let regs = info.node.regs();
    let Some(base_reg) = regs.first() else {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )));
    };

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    let mmio_base = iomap((base_reg.address as usize).into(), mmio_size as usize)?;
    let rtc = unsafe { Rtc::new(mmio_base.as_ptr().cast()) };
    let unix_timestamp = rtc.get_unix_timestamp();
    if unix_timestamp == 0 {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] returned zero unix timestamp",
            info.node.name()
        )));
    }

    let epoch_time_nanos = u64::from(unix_timestamp) * 1_000_000_000;
    if crate::generic_timer::try_init_epoch_offset(epoch_time_nanos) {
        info!("Initialized wall clock from {}", info.node.name());
    } else {
        debug!(
            "Skipping RTC {} because epoch offset is already initialized",
            info.node.name()
        );
    }

    Ok(())
}
