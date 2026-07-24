use rdrive::{
    probe::{
        OnProbeError,
        acpi::{AcpiId, ProbeAcpi},
    },
    register::ProbeKind,
};

use super::{
    cmos_decode::{
        REG_B, REG_CENTURY, REG_DAY_OF_MONTH, REG_HOURS, REG_MINUTES, REG_MONTH, REG_SECONDS,
        REG_YEAR, snapshot_to_unix_timestamp,
    },
    init_epoch_offset,
};

const REG_A: u8 = 0x0a;
const REG_A_UIP: u8 = 1 << 7;

crate::model_register!(
    name: "cmos rtc",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Acpi {
        ids: &[AcpiId {
            hid: "PNP0B00",
            cids: &[],
        }],
        on_probe: probe_cmos_acpi
    }],
);

fn probe_cmos_acpi(probe: ProbeAcpi<'_>) -> Result<(), OnProbeError> {
    let info = probe.info();
    let range = info.io_ranges().first().ok_or_else(|| {
        OnProbeError::other(alloc::format!(
            "{} has no ACPI I/O port resource",
            info.path
        ))
    })?;
    let port = u16::try_from(range.base).map_err(|_| {
        OnProbeError::other(alloc::format!(
            "{} has invalid CMOS I/O base {:#x}",
            info.path,
            range.base
        ))
    })?;
    let mut io = X86CmosIo::new(port);
    let unix_timestamp = read_unix_timestamp(&mut io);
    init_epoch_offset(info.path, unix_timestamp)
}

trait CmosIo {
    fn read(&mut self, register: u8) -> u8;
}

struct X86CmosIo {
    index_port: u16,
}

impl X86CmosIo {
    const fn new(index_port: u16) -> Self {
        Self { index_port }
    }
}

impl CmosIo for X86CmosIo {
    fn read(&mut self, register: u8) -> u8 {
        use x86::io::{inb, outb};

        unsafe {
            outb(self.index_port, register);
            inb(self.index_port + 1)
        }
    }
}

fn read_unix_timestamp(io: &mut impl CmosIo) -> u64 {
    let mut snapshot = [0u8; 128];
    for _ in 0..1_000 {
        if io.read(REG_A) & REG_A_UIP == 0 {
            read_snapshot(io, &mut snapshot);
            if io.read(REG_A) & REG_A_UIP == 0 {
                return snapshot_to_unix_timestamp(&snapshot).unwrap_or(0);
            }
        }
    }
    0
}

fn read_snapshot(io: &mut impl CmosIo, snapshot: &mut [u8; 128]) {
    for reg in [
        REG_SECONDS,
        REG_MINUTES,
        REG_HOURS,
        REG_DAY_OF_MONTH,
        REG_MONTH,
        REG_YEAR,
        REG_CENTURY,
        REG_B,
    ] {
        snapshot[reg as usize] = io.read(reg);
    }
}

#[cfg(axtest)]
pub(crate) fn cmos_register_constants_hold_for_test() -> bool {
    // Register addresses
    assert!(REG_SECONDS == 0x00);
    assert!(REG_MINUTES == 0x02);
    assert!(REG_HOURS == 0x04);
    assert!(REG_DAY_OF_MONTH == 0x07);
    assert!(REG_MONTH == 0x08);
    assert!(REG_YEAR == 0x09);
    assert!(REG_CENTURY == 0x32);
    assert!(REG_B == 0x0b);
    
    // REG_A and UIP bit
    assert!(REG_A == 0x0a);
    assert!(REG_A_UIP == 0x80);
    
    true
}

#[cfg(axtest)]
pub(crate) fn cmos_io_struct_and_constants_hold_for_test() -> bool {
    // Test X86CmosIo::new creates a valid struct
    let io = X86CmosIo::new(0x70);
    assert!(io.index_port == 0x70);
    
    let io2 = X86CmosIo::new(0x72);
    assert!(io2.index_port == 0x72);
    
    // Verify REG_A_UIP is bit 7 of REG_A
    assert!(REG_A_UIP == (1 << 7));
    
    // Register address ordering
    assert!(REG_SECONDS < REG_MINUTES);
    assert!(REG_MINUTES < REG_HOURS);
    assert!(REG_HOURS < REG_DAY_OF_MONTH);
    assert!(REG_DAY_OF_MONTH < REG_MONTH);
    assert!(REG_MONTH < REG_YEAR);
    assert!(REG_YEAR < REG_CENTURY);
    
    true
}

#[cfg(axtest)]
pub(crate) fn cmos_register_edge_cases_hold_for_test() -> bool {
    // Test specific register values
    assert_eq!(REG_SECONDS, 0x00);
    assert_eq!(REG_MINUTES, 0x02);
    assert_eq!(REG_HOURS, 0x04);
    assert_eq!(REG_DAY_OF_MONTH, 0x07);
    assert_eq!(REG_MONTH, 0x08);
    assert_eq!(REG_YEAR, 0x09);
    assert_eq!(REG_CENTURY, 0x32);
    assert_eq!(REG_B, 0x0b);
    assert_eq!(REG_A, 0x0a);
    
    // Test that register addresses are reasonable (0-127 for CMOS)
    assert!(REG_SECONDS <= 127);
    assert!(REG_CENTURY <= 127);
    
    // Test REG_A_UIP is the high bit
    assert!(REG_A_UIP == 0x80);
    assert!(REG_A_UIP > REG_A);
    
    true
}
