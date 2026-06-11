use crate::TrapFrame;

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
struct ExceptionTableEntry {
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

unsafe extern "C" {
    static _ex_table_start: [ExceptionTableEntry; 0];
    static _ex_table_end: [ExceptionTableEntry; 0];
}

impl TrapFrame {
    pub(crate) fn fixup_exception(&mut self) -> bool {
        let entries = unsafe {
            core::slice::from_raw_parts(
                _ex_table_start.as_ptr(),
                _ex_table_end
                    .as_ptr()
                    .offset_from_unsigned(_ex_table_start.as_ptr()),
            )
        };
        #[cfg(target_arch = "x86_64")]
        {
            match entries
                .iter()
                .find(|entry| entry.source_addr() == self.ip())
            {
                Some(entry) => {
                    self.set_ip(entry.to_addr());
                    true
                }
                None => false,
            }
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            match entries.binary_search_by_key(&self.ip(), ExceptionTableEntry::source_addr) {
                Ok(entry) => {
                    self.set_ip(entries[entry].to_addr());
                    true
                }
                Err(_) => false,
            }
        }
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
    }
}
