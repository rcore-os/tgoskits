use crate::TrapFrame;

#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
struct ExceptionTableEntry {
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    from: i32,
    #[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
    to: i32,
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    from: usize,
    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
    to: usize,
}

impl ExceptionTableEntry {
    #[inline]
    fn source_addr(&self) -> usize {
        #[cfg(target_arch = "aarch64")]
        {
            let base = (&self.from as *const i32) as isize;
            (base + self.from as isize) as usize
        }

        #[cfg(target_arch = "riscv64")]
        {
            let base = unsafe { _ex_table_start.as_ptr() } as isize;
            (base + self.from as isize) as usize
        }

        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            self.from
        }
    }

    #[inline]
    fn to_addr(&self) -> usize {
        #[cfg(target_arch = "aarch64")]
        {
            let base = (&self.to as *const i32) as isize;
            (base + self.to as isize) as usize
        }

        #[cfg(target_arch = "riscv64")]
        {
            let base = unsafe { _ex_table_start.as_ptr() } as isize;
            (base + self.to as isize) as usize
        }

        #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
        {
            self.to
        }
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
        match entries.binary_search_by_key(&self.ip(), ExceptionTableEntry::source_addr) {
            Ok(entry) => {
                self.set_ip(entries[entry].to_addr());
                true
            }
            Err(_) => false,
        }
    }
}

pub(crate) fn init_exception_table() {
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
