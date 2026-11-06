//! Monotonic deadlines from the firmware's RISC-V timebase.

use core::arch::asm;

use smoltcp::time::Instant;
use uefi::Status;

#[derive(Clone, Copy)]
pub(super) struct Clock {
    frequency: u64,
    origin: u64,
}

impl Clock {
    pub(super) fn open() -> uefi::Result<Self> {
        let address = uefi::system::with_config_table(|tables| {
            tables
                .iter()
                .find(|entry| entry.guid == uefi::guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0"))
                .map(|entry| entry.address)
        })
        .ok_or(Status::NOT_FOUND)?;
        // SAFETY: firmware installs a complete, retained FDT allocation under
        // the standard configuration-table GUID. The parser checks its format;
        // this borrow ends before any payload can replace firmware tables.
        let fdt = unsafe { fdt_raw::Fdt::from_ptr(address.cast_mut().cast()) }
            .map_err(|_| Status::COMPROMISED_DATA)?;
        let frequency = fdt
            .find_by_path("/cpus")
            .and_then(|node| node.find_property("timebase-frequency"))
            .and_then(|property| property.as_u32())
            .filter(|frequency| *frequency != 0)
            .ok_or(Status::UNSUPPORTED)?;
        Ok(Self {
            frequency: u64::from(frequency),
            origin: ticks(),
        })
    }

    pub(super) fn now(self) -> Instant {
        let milliseconds =
            u128::from(ticks().wrapping_sub(self.origin)) * 1000 / u128::from(self.frequency);
        Instant::from_millis(i64::try_from(milliseconds).unwrap_or(i64::MAX))
    }
}

fn ticks() -> u64 {
    let ticks;
    // SAFETY: the RISC-V UEFI platform exposes the time counter to its S-mode
    // boot image. Reading it does not modify firmware or memory state.
    unsafe { asm!("rdtime {}", out(reg) ticks, options(nomem, nostack, preserves_flags)) };
    ticks
}
