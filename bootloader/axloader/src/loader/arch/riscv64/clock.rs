//! RISC-V firmware timebase and counter access.

use core::arch::asm;

use uefi::Status;

pub(in crate::loader) fn clock_frequency() -> uefi::Result<u64> {
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
    Ok(u64::from(frequency))
}

pub(in crate::loader) fn clock_ticks() -> u64 {
    let ticks;
    // SAFETY: the RISC-V UEFI platform exposes the time counter to its S-mode
    // boot image. Reading it does not modify firmware or memory state.
    unsafe { asm!("rdtime {}", out(reg) ticks, options(nomem, nostack, preserves_flags)) };
    ticks
}
