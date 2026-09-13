//! Read-only lookup of linker-owned, relative exception recovery records.

use crate::arch::current::context::TrapFrame;

#[repr(C)]
struct ExceptionTableEntry {
    from: i32,
    to: i32,
}

impl ExceptionTableEntry {
    fn address(field: &i32, table: *const Self) -> usize {
        let base = if crate::arch::current::EX_TABLE_RELATIVE_TO_START {
            table as usize
        } else {
            field as *const i32 as usize
        };
        base.wrapping_add_signed(*field as isize)
    }
}

unsafe extern "C" {
    static _ex_table_start: [ExceptionTableEntry; 0];
    static _ex_table_end: [ExceptionTableEntry; 0];
    static _nofault_ex_table_start: [ExceptionTableEntry; 0];
    static _nofault_ex_table_end: [ExceptionTableEntry; 0];
}

/// Searches records without relocating their field-relative offsets or writing
/// shared boot state. Link order is not an instruction-address sort order.
///
/// # Safety
/// The bounds must delimit one permanently mapped, initialized linker section
/// of aligned records. Each relative target must refer to valid recovery code.
unsafe fn lookup(
    start: *const ExceptionTableEntry,
    end: *const ExceptionTableEntry,
    pc: usize,
) -> Option<usize> {
    let bytes = (end as usize)
        .checked_sub(start as usize)
        .expect("reversed exception-table linker bounds");
    assert!(bytes.is_multiple_of(core::mem::size_of::<ExceptionTableEntry>()));
    // SAFETY: the caller provides the complete stable linker section; no CPU
    // ever mutates it, so concurrent exception lookup needs no initialization lock.
    let entries = unsafe {
        core::slice::from_raw_parts(start, bytes / core::mem::size_of::<ExceptionTableEntry>())
    };
    entries.iter().find_map(|entry| {
        (ExceptionTableEntry::address(&entry.from, start) == pc)
            .then(|| ExceptionTableEntry::address(&entry.to, start))
    })
}

impl TrapFrame {
    pub(crate) fn fixup_nofault_exception(&mut self) -> bool {
        // SAFETY: the image linker retains this aligned read-only record section
        // and the CPU assembly emits offsets into its permanently mapped text.
        let recovery = unsafe {
            lookup(
                _nofault_ex_table_start.as_ptr(),
                _nofault_ex_table_end.as_ptr(),
                self.ip(),
            )
        };
        if let Some(pc) = recovery {
            self.set_ip(pc);
        }
        recovery.is_some()
    }

    pub(crate) fn fixup_exception(&mut self) -> bool {
        // SAFETY: these symbols delimit the second section with the same record
        // and mapping contract; lookup preserves the architecture's offset base.
        let recovery =
            unsafe { lookup(_ex_table_start.as_ptr(), _ex_table_end.as_ptr(), self.ip()) };
        if let Some(pc) = recovery {
            self.set_ip(pc);
        }
        recovery.is_some()
    }
}
