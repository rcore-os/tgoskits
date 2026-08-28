use crate::TrapFrame;

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
struct ExceptionTableEntry {
    from: i32,
    to: i32,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
struct NofaultExceptionTableEntry {
    from: i32,
    to: i32,
}

impl ExceptionTableEntry {
    #[inline]
    fn source_addr(&self) -> usize {
        exception_addr(&self.from)
    }

    #[inline]
    fn to_addr(&self) -> usize {
        exception_addr(&self.to)
    }
}

impl NofaultExceptionTableEntry {
    #[inline]
    fn source_addr(&self) -> usize {
        nofault_exception_addr(&self.from)
    }

    #[inline]
    fn to_addr(&self) -> usize {
        nofault_exception_addr(&self.to)
    }
}

#[inline]
fn exception_addr(offset: &i32) -> usize {
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    ))]
    {
        let base = (offset as *const i32) as isize;
        (base + *offset as isize) as usize
    }

    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    {
        let base = unsafe { _ex_table_start.as_ptr() } as isize;
        (base + *offset as isize) as usize
    }

    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "x86_64"
    )))]
    {
        *offset as usize
    }
}

#[inline]
fn nofault_exception_addr(offset: &i32) -> usize {
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    ))]
    {
        let base = (offset as *const i32) as isize;
        (base + *offset as isize) as usize
    }

    #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))]
    {
        let base = unsafe { _nofault_ex_table_start.as_ptr() } as isize;
        (base + *offset as isize) as usize
    }
}

unsafe extern "C" {
    static _ex_table_start: [ExceptionTableEntry; 0];
    static _ex_table_end: [ExceptionTableEntry; 0];
    static _nofault_ex_table_start: [NofaultExceptionTableEntry; 0];
    static _nofault_ex_table_end: [NofaultExceptionTableEntry; 0];
}

impl TrapFrame {
    #[cfg(not(target_arch = "aarch64"))]
    pub(crate) fn fixup_nofault_exception(&mut self) -> bool {
        let mut ip = self.ip();
        if fixup_nofault_exception_ip(&mut ip) {
            self.set_ip(ip);
            true
        } else {
            false
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    pub(crate) fn fixup_exception(&mut self) -> bool {
        let mut ip = self.ip();
        if fixup_exception_ip(&mut ip) {
            self.set_ip(ip);
            true
        } else {
            false
        }
    }
}

pub(crate) fn fixup_nofault_exception_ip(ip: &mut usize) -> bool {
    // SAFETY: the linker emits `_nofault_ex_table_start` and
    // `_nofault_ex_table_end` as properly aligned bounds of one contiguous
    // `NofaultExceptionTableEntry` section, with start no later than end.
    // Both symbols therefore belong to the same linker-defined allocation and
    // the computed element range is exactly the initialized table.
    let entries = unsafe {
        core::slice::from_raw_parts(
            _nofault_ex_table_start.as_ptr(),
            _nofault_ex_table_end
                .as_ptr()
                .offset_from_unsigned(_nofault_ex_table_start.as_ptr()),
        )
    };
    let target = {
        #[cfg(target_arch = "x86_64")]
        {
            entries
                .iter()
                .find(|entry| entry.source_addr() == *ip)
                .map(NofaultExceptionTableEntry::to_addr)
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            match entries.binary_search_by_key(ip, NofaultExceptionTableEntry::source_addr) {
                Ok(entry) => Some(entries[entry].to_addr()),
                Err(_) => None,
            }
        }
    };
    if let Some(target) = target {
        *ip = target;
        true
    } else {
        false
    }
}

pub(crate) fn fixup_exception_ip(ip: &mut usize) -> bool {
    // SAFETY: the linker emits `_ex_table_start` and `_ex_table_end` as
    // properly aligned bounds of one contiguous `ExceptionTableEntry`
    // section, with start no later than end. Both symbols therefore belong to
    // the same linker-defined allocation and the computed element range is
    // exactly the initialized table.
    let entries = unsafe {
        core::slice::from_raw_parts(
            _ex_table_start.as_ptr(),
            _ex_table_end
                .as_ptr()
                .offset_from_unsigned(_ex_table_start.as_ptr()),
        )
    };
    let target = {
        #[cfg(target_arch = "x86_64")]
        {
            entries
                .iter()
                .find(|entry| entry.source_addr() == *ip)
                .map(ExceptionTableEntry::to_addr)
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            match entries.binary_search_by_key(ip, ExceptionTableEntry::source_addr) {
                Ok(entry) => Some(entries[entry].to_addr()),
                Err(_) => None,
            }
        }
    };
    if let Some(target) = target {
        *ip = target;
        true
    } else {
        false
    }
}

pub(crate) fn init_exception_table() {
    #[cfg(not(target_arch = "x86_64"))]
    {
        let ex_table = unsafe {
            core::slice::from_raw_parts_mut(
                _ex_table_start.as_ptr().cast_mut(),
                _ex_table_end
                    .as_ptr()
                    .offset_from_unsigned(_ex_table_start.as_ptr()),
            )
        };
        ex_table.sort_unstable_by_key(ExceptionTableEntry::source_addr);
        let nofault_ex_table = unsafe {
            core::slice::from_raw_parts_mut(
                _nofault_ex_table_start.as_ptr().cast_mut(),
                _nofault_ex_table_end
                    .as_ptr()
                    .offset_from_unsigned(_nofault_ex_table_start.as_ptr()),
            )
        };
        nofault_ex_table.sort_unstable_by_key(NofaultExceptionTableEntry::source_addr);
    }
}
