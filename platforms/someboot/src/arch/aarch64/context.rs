use core::fmt::{self, Debug};

#[repr(C, align(0x10))]
#[derive(Clone)]
pub struct Context {
    pub sp: *const u8,
    pub pc: *const u8,
    #[cfg(hard_float)]
    /// Floating-point Control Register (FPCR)
    pub fpcr: usize,
    #[cfg(hard_float)]
    /// Floating-point Status Register (FPSR)
    pub fpsr: usize,
    #[cfg(hard_float)]
    pub q: [u128; 32],
    pub spsr: u64,
    pub x: [usize; 30],
    pub lr: *const u8,
}

unsafe impl Send for Context {}

impl Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Context:")?;

        const NUM_CHUNKS: usize = 4;

        for (r, chunk) in self.x.chunks(NUM_CHUNKS).enumerate() {
            let row_start = r * NUM_CHUNKS;

            for (i, v) in chunk.iter().enumerate() {
                let i = row_start + i;
                write!(f, "  x{i:<3}: {v:#18x}")?;
            }
            writeln!(f)?;
        }
        writeln!(f, "  lr  : {:p}", self.lr)?;
        writeln!(f, "  spsr: {:#18x}", self.spsr)?;
        writeln!(f, "  pc  : {:p}", self.pc)?;
        writeln!(f, "  sp  : {:p}", self.sp)
    }
}
