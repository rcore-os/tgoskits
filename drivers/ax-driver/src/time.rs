#[cfg(target_arch = "aarch64")]
use ax_arm_pl031::Rtc as Pl031Rtc;
use log::{debug, info};
use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};
#[cfg(target_arch = "riscv64")]
use riscv_goldfish::Rtc as GoldfishRtc;

use crate::mmio::iomap;

#[cfg(target_arch = "aarch64")]
crate::model_register!(
    name: "pl031 rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl031"],
        on_probe: probe_pl031
    }],
);

#[cfg(target_arch = "riscv64")]
crate::model_register!(
    name: "goldfish rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["google,goldfish-rtc"],
        on_probe: probe_goldfish
    }],
);

#[cfg(target_arch = "aarch64")]
fn probe_pl031(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let rtc = unsafe { Pl031Rtc::new(mmio_base.as_ptr().cast()) };
    init_epoch_offset(info.node.name(), u64::from(rtc.get_unix_timestamp()))
}

#[cfg(target_arch = "riscv64")]
fn probe_goldfish(probe: ProbeFdt<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let rtc = GoldfishRtc::new(mmio_base.as_ptr() as usize);
    init_epoch_offset(info.node.name(), rtc.get_unix_timestamp())
}

fn map_first_reg(info: &FdtInfo<'_>) -> Result<core::ptr::NonNull<u8>, OnProbeError> {
    let regs = info.node.regs();
    let Some(base_reg) = regs.first() else {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )));
    };

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    iomap(base_reg.address as usize, mmio_size as usize)
}

fn init_epoch_offset(node_name: &str, unix_timestamp: u64) -> Result<(), OnProbeError> {
    if unix_timestamp == 0 {
        return Err(OnProbeError::other(alloc::format!(
            "[{node_name}] returned zero unix timestamp"
        )));
    }

    let epoch_time_nanos = unix_timestamp * 1_000_000_000;
    if axklib::time::try_init_epoch_offset(epoch_time_nanos) {
        info!("Initialized wall clock from {node_name}");
    } else {
        debug!("Skipping RTC {node_name} because epoch offset is already initialized",);
    }

    Ok(())
}
