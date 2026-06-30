//! NS16550 MMIO 版本实现
//!
//! 适用于嵌入式平台的内存映射 IO 版本

use core::ptr::NonNull;

use super::{Kind, Ns16550};

#[derive(Clone)]
pub struct Mmio {
    base: usize,
    width: usize,
}

impl Kind for Mmio {
    fn read_reg(&self, reg: u8) -> u8 {
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *const u8).read_volatile()
        }
    }

    fn write_reg(&self, reg: u8, val: u8) {
        unsafe {
            let addr = self.base + (reg as usize) * self.width;
            (addr as *mut u8).write_volatile(val);
        }
    }

    fn get_base(&self) -> usize {
        self.base
    }
}

impl Ns16550<Mmio> {
    pub fn new_mmio(base: NonNull<u8>, clock_freq: u32, reg_width: usize) -> Ns16550<Mmio> {
        let base = Mmio {
            base: base.as_ptr() as usize,
            width: reg_width,
        };

        Ns16550 {
            base,
            clock_freq,
            saved_lsr: super::registers::LineStatusFlags::empty(),
        }
    }
}
