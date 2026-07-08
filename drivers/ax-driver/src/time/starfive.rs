use core::ptr::read_volatile;

use rdrive::probe::OnProbeError;

use super::{
    fdt::{FdtProbe, map_first_reg},
    init_epoch_offset,
    starfive_decode::decode_rtc_datetime,
};

const RTC_TIME_OFFSET: usize = 0x3c;
const RTC_DATE_OFFSET: usize = 0x40;

crate::model_register!(
    name: "StarFive JH7110 RTC",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["starfive,jh7110-rtc"],
            on_probe: probe
        }
    ],
);

fn probe(probe: FdtProbe<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let unix_timestamp = read_unix_timestamp(info.node.name(), mmio_base.as_ptr())?;
    init_epoch_offset(info.node.name(), unix_timestamp)
}

fn read_unix_timestamp(node_name: &str, base: *mut u8) -> Result<u64, OnProbeError> {
    let time_reg = unsafe { read_volatile(base.add(RTC_TIME_OFFSET).cast::<u32>()) };
    let date_reg = unsafe { read_volatile(base.add(RTC_DATE_OFFSET).cast::<u32>()) };

    decode_rtc_datetime(time_reg, date_reg).ok_or_else(|| {
        OnProbeError::other(alloc::format!(
            "[{node_name}] has invalid RTC time/date registers: time={time_reg:#010x}, \
             date={date_reg:#010x}"
        ))
    })
}
