use rdrive::{
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
    },
    register::ProbeKind,
};

use super::{fdt::FdtProbe, init_epoch_offset, loongson_decode::toy_to_unix_timestamp};
use crate::mmio::{firmware_addr_to_phys, iomap_firmware_device};

crate::model_register!(
    name: "loongson rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Fdt {
            compatibles: &[
                "loongson,ls7a-rtc",
                "loongson,ls2k1000-rtc",
                "loongson,ls-rtc",
            ],
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
    let mmio_base = map_loongson_fdt_reg(info)?;
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
    const SYS_TOYTRIM: usize = 0x20;
    const SYS_TOY_READ0: usize = 0x2c;
    const SYS_TOY_READ1: usize = 0x30;
    const SYS_RTCCTRL: usize = 0x40;
    const SYS_RTCTRIM: usize = 0x60;
    const RTC_ENABLE: u32 = 1 << 13;
    const TOY_ENABLE: u32 = 1 << 11;
    const OSC_ENABLE: u32 = 1 << 8;
    const ENABLE_MASK: u32 = RTC_ENABLE | TOY_ENABLE | OSC_ENABLE;

    let toy_trim = unsafe { (base.add(SYS_TOYTRIM) as *const u32).read_volatile() };
    let rtc_trim = unsafe { (base.add(SYS_RTCTRIM) as *const u32).read_volatile() };
    unsafe {
        (base.add(SYS_TOYTRIM) as *mut u32).write_volatile(0);
        (base.add(SYS_RTCTRIM) as *mut u32).write_volatile(0);
    }
    let ctrl = unsafe { (base.add(SYS_RTCCTRL) as *const u32).read_volatile() };
    unsafe {
        (base.add(SYS_RTCCTRL) as *mut u32).write_volatile(ctrl | ENABLE_MASK);
    }
    let ctrl_after = unsafe { (base.add(SYS_RTCCTRL) as *const u32).read_volatile() };

    let mut last_toy_low = 0;
    let mut last_toy_high = 0;
    for _ in 0..3 {
        // Loongson's Linux RTC driver reads TOY_READ0 followed by TOY_READ1.
        // Keep the same order so hardware can latch a consistent TOY snapshot.
        let toy_low = unsafe { (base.add(SYS_TOY_READ0) as *const u32).read_volatile() };
        let toy_high = unsafe { (base.add(SYS_TOY_READ1) as *const u32).read_volatile() };
        last_toy_low = toy_low;
        last_toy_high = toy_high;
        if let Some(unix_timestamp) = toy_to_unix_timestamp(toy_high, toy_low) {
            return unix_timestamp;
        }
        core::hint::spin_loop();
    }

    log::warn!(
        "Loongson RTC returned invalid TOY value: ctrl={ctrl:#010x}->{ctrl_after:#010x}, \
         toy_trim={toy_trim:#010x}, rtc_trim={rtc_trim:#010x}, toy_high={last_toy_high:#010x}, \
         toy_low={last_toy_low:#010x}"
    );
    0
}

fn map_loongson_fdt_reg(
    info: &rdrive::register::FdtInfo<'_>,
) -> Result<core::ptr::NonNull<u8>, OnProbeError> {
    let regs = info.node.regs();
    let Some(base_reg) = regs.first() else {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )));
    };

    let fw_addr = base_reg.address as usize;
    let paddr = firmware_addr_to_phys(fw_addr);
    let size = base_reg.size.unwrap_or(0x1000) as usize;
    let mmio = iomap_firmware_device("loongson rtc", fw_addr, size)?;
    log::debug!(
        "probing loongson rtc: node={}, reg={fw_addr:#x}, paddr={paddr:#x}, vaddr={:#x}, \
         size={size:#x}",
        info.node.name(),
        mmio.as_ptr() as usize,
    );
    Ok(mmio)
}
