use ax_arm_pl031::Rtc as Pl031Rtc;
use rdrive::probe::OnProbeError;

use super::{
    fdt::{FdtProbe, map_first_reg},
    init_epoch_offset,
};

crate::model_register!(
    name: "pl031 rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["arm,pl031"],
        on_probe: probe_pl031
    }],
);

fn probe_pl031(probe: FdtProbe<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let rtc = unsafe { Pl031Rtc::new(mmio_base.as_ptr().cast()) };
    init_epoch_offset(info.node.name(), u64::from(rtc.get_unix_timestamp()))
}
