use rdrive::probe::OnProbeError;
use riscv_goldfish::Rtc as GoldfishRtc;

use super::{
    fdt::{FdtProbe, map_first_reg},
    init_epoch_offset,
};

crate::model_register!(
    name: "goldfish rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Fdt {
        compatibles: &["google,goldfish-rtc"],
        on_probe: probe_goldfish
    }],
);

fn probe_goldfish(probe: FdtProbe<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let rtc = GoldfishRtc::new(mmio_base.as_ptr() as usize);
    init_epoch_offset(info.node.name(), rtc.get_unix_timestamp())
}
