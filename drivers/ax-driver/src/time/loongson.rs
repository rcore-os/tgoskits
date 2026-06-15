use rdrive::{
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
    },
    register::ProbeKind,
};

use super::{
    fdt::{FdtProbe, map_first_reg},
    init_epoch_offset,
    loongson_decode::toy_to_unix_timestamp,
};

crate::model_register!(
    name: "loongson rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &["loongson,ls7a-rtc"],
            on_probe: probe_loongson_fdt
        },
        ProbeKind::Acpi {
            ids: &[AcpiId {
                hid: "LOON0001",
                cids: &[],
            }],
            on_probe: probe_loongson_acpi
        },
    ],
);

fn probe_loongson_fdt(probe: FdtProbe<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let mmio_base = map_first_reg(info)?;
    let unix_timestamp = read_unix_timestamp(mmio_base.as_ptr());
    init_epoch_offset(info.node.name(), unix_timestamp)
}

fn probe_loongson_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let range = info.memory_ranges().first().ok_or_else(|| {
        OnProbeError::other(alloc::format!("{} has no ACPI MMIO resource", info.path))
    })?;
    let size = usize::try_from(range.size).unwrap_or(0x100).max(0x100);
    let base = usize::try_from(range.base).map_err(|_| {
        OnProbeError::other(alloc::format!(
            "{} has invalid ACPI MMIO base {:#x}",
            info.path,
            range.base
        ))
    })?;
    let mmio_base = crate::mmio::iomap(base, size)?;
    let unix_timestamp = read_unix_timestamp(mmio_base.as_ptr());
    init_epoch_offset(info.path, unix_timestamp)
}

fn read_unix_timestamp(base: *mut u8) -> u64 {
    const SYS_TOY_READ0: usize = 0x2c;
    const SYS_TOY_READ1: usize = 0x30;
    const SYS_RTCCTRL: usize = 0x40;
    const TOY_ENABLE: u32 = 1 << 11;
    const OSC_ENABLE: u32 = 1 << 8;

    unsafe {
        (base.add(SYS_RTCCTRL) as *mut u32).write_volatile(TOY_ENABLE | OSC_ENABLE);
    }
    let toy_high = unsafe { (base.add(SYS_TOY_READ1) as *const u32).read_volatile() };
    let toy_low = unsafe { (base.add(SYS_TOY_READ0) as *const u32).read_volatile() };
    toy_to_unix_timestamp(toy_high, toy_low).unwrap_or(0)
}
