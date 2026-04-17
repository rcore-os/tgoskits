use core::fmt::{self, Formatter};

#[allow(dead_code)]
#[allow(missing_docs)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GprIndex {
    R0 = 0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    R16,
    R17,
    R18,
    R19,
    R20,
    R21,
    R22,
    R23,
    R24,
    R25,
    R26,
    R27,
    R28,
    R29,
    R30,
    R31,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LoongArchContextFrame {
    pub x: [usize; 32],
    pub sepc: usize,
    pub crmd: usize,
    pub prmd: usize,
    pub estat: usize,
}

impl LoongArchContextFrame {
    pub fn set_argument(&mut self, arg: usize) {
        self.x[4] = arg;
    }

    pub fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0 => {}
            1..=31 => self.x[index] = val,
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }

    pub fn gpr(&self, index: usize) -> usize {
        match index {
            0 => 0,
            1..=31 => self.x[index],
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }

    pub fn get_a0(&self) -> usize {
        self.x[4]
    }

    pub fn get_a1(&self) -> usize {
        self.x[5]
    }

    pub fn get_a2(&self) -> usize {
        self.x[6]
    }

    pub fn get_a3(&self) -> usize {
        self.x[7]
    }

    pub fn get_a4(&self) -> usize {
        self.x[8]
    }

    pub fn get_a5(&self) -> usize {
        self.x[9]
    }

    pub fn get_a6(&self) -> usize {
        self.x[10]
    }

    pub fn set_a0(&mut self, val: usize) {
        self.x[4] = val;
    }
}

impl fmt::Display for LoongArchContextFrame {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (idx, value) in self.x.iter().enumerate() {
            write!(f, "x{idx:02}: {value:016x}   ")?;
            if (idx + 1) % 2 == 0 {
                writeln!(f)?;
            }
        }
        writeln!(f, "sepc: {:016x}", self.sepc)?;
        writeln!(f, "crmd: {:016x}", self.crmd)?;
        writeln!(f, "prmd: {:016x}", self.prmd)?;
        write!(f, "estat: {:016x}", self.estat)
    }
}

#[repr(C)]
#[repr(align(16))]
#[derive(Debug, Clone, Copy, Default)]
pub struct LoongArchGuestSystemRegisters {
    pub gpgd: usize,
    pub gpgdl: usize,
    pub gpgdh: usize,
    pub gasid: usize,
    pub gtcfg: usize,
    pub gtval: usize,
    pub gticlr: usize,
    pub gtlbehi: usize,
    pub gtlbelo0: usize,
    pub gtlbelo1: usize,
    pub gtlbidx: usize,
    pub gstat: usize,
    pub gctl: usize,
    pub geentry: usize,
    pub gera: usize,
    pub gbadv: usize,
    pub gbadi: usize,
}

impl LoongArchGuestSystemRegisters {
    /// Stores guest system registers into this software snapshot.
    ///
    /// # Safety
    ///
    /// The caller must ensure the current CPU is in the correct host context to
    /// access guest system-register state.
    pub unsafe fn store(&mut self) {
        use crate::registers::*;

        self.gpgd = gcsr_read::<GCSR_PGD>();
        self.gpgdl = gcsr_read::<GCSR_PGDL>();
        self.gpgdh = gcsr_read::<GCSR_PGDH>();
        self.gasid = gcsr_read::<GCSR_ASID>();
        self.gtcfg = gcsr_read::<GCSR_TCFG>();
        self.gtval = gcsr_read::<GCSR_TVAL>();
        self.gticlr = gcsr_read::<GCSR_TICLR>();
        self.gtlbehi = gcsr_read::<GCSR_TLBEHI>();
        self.gtlbelo0 = gcsr_read::<GCSR_TLBELO0>();
        self.gtlbelo1 = gcsr_read::<GCSR_TLBELO1>();
        self.gtlbidx = gcsr_read::<GCSR_TLBIDX>();
        self.gstat = gstat_read();
        self.gera = gcsr_read::<GCSR_ERA>();
        self.geentry = gcsr_read::<GCSR_EENTRY>();
        self.gbadv = gcsr_read::<GCSR_BADV>();
        self.gbadi = gcsr_read::<GCSR_BADI>();
        self.gctl = csr_read::<CSR_GCTL>();
    }

    /// Restores guest system registers from this software snapshot.
    ///
    /// # Safety
    ///
    /// The caller must ensure the current CPU is ready to accept guest
    /// system-register state before invoking this routine.
    pub unsafe fn restore(&self) {
        use crate::registers::*;

        gcsr_write::<GCSR_PGD>(self.gpgd);
        gcsr_write::<GCSR_PGDL>(self.gpgdl);
        gcsr_write::<GCSR_PGDH>(self.gpgdh);
        gcsr_write::<GCSR_ASID>(self.gasid);
        gcsr_write::<GCSR_TCFG>(self.gtcfg);
        gcsr_write::<GCSR_TVAL>(self.gtval);
        gcsr_write::<GCSR_TICLR>(self.gticlr);
        gcsr_write::<GCSR_TLBEHI>(self.gtlbehi);
        gcsr_write::<GCSR_TLBELO0>(self.gtlbelo0);
        gcsr_write::<GCSR_TLBELO1>(self.gtlbelo1);
        gcsr_write::<GCSR_TLBIDX>(self.gtlbidx);
        gcsr_write::<GCSR_ERA>(self.gera);
        gcsr_write::<GCSR_EENTRY>(self.geentry);
        csr_write::<CSR_GCTL>(self.gctl);
    }
}
