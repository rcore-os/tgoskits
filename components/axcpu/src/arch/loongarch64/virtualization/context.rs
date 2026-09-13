//! LVZ guest register image.

use core::fmt::{self, Formatter};

/// Integer, guest CSR and saved host translation image used by LVZ entry.
#[repr(C)]
#[repr(align(16))]
#[derive(Clone, Copy, Debug, Default)]
pub struct GuestContext {
    /// General-purpose registers, in architectural register-number order.
    pub x: crate::registers::GeneralRegisters,
    /// Saved `sepc` register value.
    pub sepc: usize,
    /// Saved `gcsr_crmd` register value.
    pub gcsr_crmd: usize,
    /// Saved `gcsr_prmd` register value.
    pub gcsr_prmd: usize,
    /// Saved `gcsr_euen` register value.
    pub gcsr_euen: usize,
    /// Saved `gcsr_misc` register value.
    pub gcsr_misc: usize,
    /// Saved `gcsr_ectl` register value.
    pub gcsr_ectl: usize,
    /// Saved `gcsr_estat` register value.
    pub gcsr_estat: usize,
    /// Saved `gcsr_era` register value.
    pub gcsr_era: usize,
    /// Saved `gcsr_badv` register value.
    pub gcsr_badv: usize,
    /// Saved `gcsr_badi` register value.
    pub gcsr_badi: usize,
    /// Saved `gcsr_eentry` register value.
    pub gcsr_eentry: usize,
    /// Saved `gcsr_tlbidx` register value.
    pub gcsr_tlbidx: usize,
    /// Saved `gcsr_tlbehi` register value.
    pub gcsr_tlbehi: usize,
    /// Saved `gcsr_tlbelo0` register value.
    pub gcsr_tlbelo0: usize,
    /// Saved `gcsr_tlbelo1` register value.
    pub gcsr_tlbelo1: usize,
    /// Saved `gcsr_asid` register value.
    pub gcsr_asid: usize,
    /// Saved `gcsr_pgdl` register value.
    pub gcsr_pgdl: usize,
    /// Saved `gcsr_pgdh` register value.
    pub gcsr_pgdh: usize,
    /// Saved `gcsr_pgd` register value.
    pub gcsr_pgd: usize,
    /// Saved `gcsr_pwcl` register value.
    pub gcsr_pwcl: usize,
    /// Saved `gcsr_pwch` register value.
    pub gcsr_pwch: usize,
    /// Saved `gcsr_stlbps` register value.
    pub gcsr_stlbps: usize,
    /// Saved `gcsr_ravcfg` register value.
    pub gcsr_ravcfg: usize,
    /// Saved `gcsr_cpuid` register value.
    pub gcsr_cpuid: usize,
    /// Saved `gcsr_prcfg1` register value.
    pub gcsr_prcfg1: usize,
    /// Saved `gcsr_prcfg2` register value.
    pub gcsr_prcfg2: usize,
    /// Saved `gcsr_prcfg3` register value.
    pub gcsr_prcfg3: usize,
    /// Saved `gcsr_save0` register value.
    pub gcsr_save0: usize,
    /// Saved `gcsr_save1` register value.
    pub gcsr_save1: usize,
    /// Saved `gcsr_save2` register value.
    pub gcsr_save2: usize,
    /// Saved `gcsr_save3` register value.
    pub gcsr_save3: usize,
    /// Saved `gcsr_save4` register value.
    pub gcsr_save4: usize,
    /// Saved `gcsr_save5` register value.
    pub gcsr_save5: usize,
    /// Saved `gcsr_save6` register value.
    pub gcsr_save6: usize,
    /// Saved `gcsr_save7` register value.
    pub gcsr_save7: usize,
    /// Saved `gcsr_save8` register value.
    pub gcsr_save8: usize,
    /// Saved `gcsr_save9` register value.
    pub gcsr_save9: usize,
    /// Saved `gcsr_save10` register value.
    pub gcsr_save10: usize,
    /// Saved `gcsr_save11` register value.
    pub gcsr_save11: usize,
    /// Saved `gcsr_save12` register value.
    pub gcsr_save12: usize,
    /// Saved `gcsr_save13` register value.
    pub gcsr_save13: usize,
    /// Saved `gcsr_save14` register value.
    pub gcsr_save14: usize,
    /// Saved `gcsr_save15` register value.
    pub gcsr_save15: usize,
    /// Saved `gcsr_tid` register value.
    pub gcsr_tid: usize,
    /// Saved `gcsr_tcfg` register value.
    pub gcsr_tcfg: usize,
    /// Saved `gcsr_tval` register value.
    pub gcsr_tval: usize,
    /// Saved `gcsr_cntc` register value.
    pub gcsr_cntc: usize,
    /// Saved `gcsr_ticlr` register value.
    pub gcsr_ticlr: usize,
    /// Saved `gcsr_llbctl` register value.
    pub gcsr_llbctl: usize,
    /// Saved `gcsr_tlbrentry` register value.
    pub gcsr_tlbrentry: usize,
    /// Saved `gcsr_tlbrbadv` register value.
    pub gcsr_tlbrbadv: usize,
    /// Saved `gcsr_tlbrera` register value.
    pub gcsr_tlbrera: usize,
    /// Saved `gcsr_tlbrsave` register value.
    pub gcsr_tlbrsave: usize,
    /// Saved `gcsr_tlbrelo0` register value.
    pub gcsr_tlbrelo0: usize,
    /// Saved `gcsr_tlbrelo1` register value.
    pub gcsr_tlbrelo1: usize,
    /// Saved `gcsr_tlbrehi` register value.
    pub gcsr_tlbrehi: usize,
    /// Saved `gcsr_tlbrprmd` register value.
    pub gcsr_tlbrprmd: usize,
    /// Saved `gcsr_dmw0` register value.
    pub gcsr_dmw0: usize,
    /// Saved `gcsr_dmw1` register value.
    pub gcsr_dmw1: usize,
    /// Saved `gcsr_dmw2` register value.
    pub gcsr_dmw2: usize,
    /// Saved `gcsr_dmw3` register value.
    pub gcsr_dmw3: usize,
    /// Saved `host_estat` register value.
    pub host_estat: usize,
    /// Saved `host_era` register value.
    pub host_era: usize,
    /// Saved `host_badv` register value.
    pub host_badv: usize,
    /// Saved `host_badi` register value.
    pub host_badi: usize,
    /// Saved `host_tlbrbadv` register value.
    pub host_tlbrbadv: usize,
    /// Saved `host_tlbrera` register value.
    pub host_tlbrera: usize,
    /// Saved `host_pgdl` register value.
    pub host_pgdl: usize,
    /// Saved `host_pgdh` register value.
    pub host_pgdh: usize,
    /// Saved `host_pwcl` register value.
    pub host_pwcl: usize,
    /// Saved `host_pwch` register value.
    pub host_pwch: usize,
    /// Saved `host_stlbps` register value.
    pub host_stlbps: usize,
    /// Saved `host_tlbrentry` register value.
    pub host_tlbrentry: usize,
    /// Saved `host_asid` register value.
    pub host_asid: usize,
    /// Saved `host_eentry` register value.
    pub host_eentry: usize,
    /// Saved `host_ecfg` register value.
    pub host_ecfg: usize,
    /// Saved `guest_tlbrentry` register value.
    pub guest_tlbrentry: usize,
    /// Saved `guest_eentry` register value.
    pub guest_eentry: usize,
}

impl GuestContext {
    /// Sets the first argument register.
    pub fn set_argument(&mut self, arg: usize) {
        self.x[4] = arg;
    }

    /// Sets argument register a1.
    pub fn set_a1(&mut self, val: usize) {
        self.x[5] = val;
    }

    /// Sets argument register a2.
    pub fn set_a2(&mut self, val: usize) {
        self.x[6] = val;
    }

    /// Writes an integer register; writes to r0 are ignored.
    pub fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0 => {}
            1..=31 => self.x[index] = val,
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }

    /// Reads an integer register; r0 always reads as zero.
    pub fn gpr(&self, index: usize) -> usize {
        match index {
            0 => 0,
            1..=31 => self.x[index],
            _ => panic!("invalid general-purpose register index {index}"),
        }
    }

    /// Returns the saved faulting PC, including TLB refill exits.
    pub fn guest_exception_pc(&self) -> usize {
        if self.host_tlbrera & 0x1 != 0 {
            self.host_tlbrera & !0x1
        } else if self.host_era != 0 {
            self.host_era
        } else {
            self.gcsr_era
        }
    }

    /// Advances the saved guest PC past one instruction.
    pub fn advance_guest_pc(&mut self) {
        let next_pc = self.guest_exception_pc().wrapping_add(4);
        self.sepc = next_pc;
        self.gcsr_era = next_pc;
        self.host_era = next_pc;
        self.host_tlbrera = next_pc;
    }

    /// Reads argument register a0.
    pub fn get_a0(&self) -> usize {
        self.x[4]
    }

    /// Reads argument register a1.
    pub fn get_a1(&self) -> usize {
        self.x[5]
    }

    /// Reads argument register a2.
    pub fn get_a2(&self) -> usize {
        self.x[6]
    }

    /// Reads argument register a3.
    pub fn get_a3(&self) -> usize {
        self.x[7]
    }

    /// Reads argument register a4.
    pub fn get_a4(&self) -> usize {
        self.x[8]
    }

    /// Reads argument register a5.
    pub fn get_a5(&self) -> usize {
        self.x[9]
    }

    /// Reads argument register a6.
    pub fn get_a6(&self) -> usize {
        self.x[10]
    }

    /// Sets argument register a0.
    pub fn set_a0(&mut self, val: usize) {
        self.x[4] = val;
    }
}

impl fmt::Display for GuestContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for idx in 0..32 {
            let value = self.x[idx];
            write!(f, "x{idx:02}: {value:016x}   ")?;
            if (idx + 1) % 2 == 0 {
                writeln!(f)?;
            }
        }
        writeln!(f, "sepc: {:016x}", self.sepc)?;
        writeln!(f, "gcsr_crmd: {:016x}", self.gcsr_crmd)?;
        writeln!(f, "gcsr_prmd: {:016x}", self.gcsr_prmd)?;
        writeln!(f, "gcsr_euen: {:016x}", self.gcsr_euen)?;
        writeln!(f, "gcsr_misc: {:016x}", self.gcsr_misc)?;
        writeln!(f, "gcsr_ectl: {:016x}", self.gcsr_ectl)?;
        writeln!(f, "gcsr_estat: {:016x}", self.gcsr_estat)?;
        writeln!(f, "gcsr_era: {:016x}", self.gcsr_era)?;
        writeln!(f, "gcsr_badv: {:016x}", self.gcsr_badv)?;
        writeln!(f, "gcsr_badi: {:016x}", self.gcsr_badi)?;
        writeln!(f, "gcsr_eentry: {:016x}", self.gcsr_eentry)?;
        writeln!(f, "gcsr_tlbidx: {:016x}", self.gcsr_tlbidx)?;
        writeln!(f, "gcsr_tlbehi: {:016x}", self.gcsr_tlbehi)?;
        writeln!(f, "gcsr_tlbelo0: {:016x}", self.gcsr_tlbelo0)?;
        writeln!(f, "gcsr_tlbelo1: {:016x}", self.gcsr_tlbelo1)?;
        writeln!(f, "gcsr_asid: {:016x}", self.gcsr_asid)?;
        writeln!(f, "gcsr_pgd: {:016x}", self.gcsr_pgd)?;
        writeln!(f, "gcsr_pgdl: {:016x}", self.gcsr_pgdl)?;
        writeln!(f, "gcsr_pgdh: {:016x}", self.gcsr_pgdh)?;
        writeln!(f, "gcsr_pwcl: {:016x}", self.gcsr_pwcl)?;
        writeln!(f, "gcsr_pwch: {:016x}", self.gcsr_pwch)?;
        writeln!(f, "gcsr_stlbps: {:016x}", self.gcsr_stlbps)?;
        writeln!(f, "gcsr_tcfg: {:016x}", self.gcsr_tcfg)?;
        writeln!(f, "gcsr_tval: {:016x}", self.gcsr_tval)?;
        writeln!(f, "gcsr_ticlr: {:016x}", self.gcsr_ticlr)?;
        writeln!(f, "gcsr_tlbrentry: {:016x}", self.gcsr_tlbrentry)?;
        writeln!(f, "gcsr_tlbrbadv: {:016x}", self.gcsr_tlbrbadv)?;
        writeln!(f, "gcsr_tlbrera: {:016x}", self.gcsr_tlbrera)?;
        writeln!(f, "gcsr_tlbrelo0: {:016x}", self.gcsr_tlbrelo0)?;
        writeln!(f, "gcsr_tlbrelo1: {:016x}", self.gcsr_tlbrelo1)?;
        writeln!(f, "gcsr_tlbrehi: {:016x}", self.gcsr_tlbrehi)?;
        writeln!(f, "gcsr_tlbrprmd: {:016x}", self.gcsr_tlbrprmd)?;
        writeln!(f, "gcsr_dmw0: {:016x}", self.gcsr_dmw0)?;
        writeln!(f, "gcsr_dmw1: {:016x}", self.gcsr_dmw1)?;
        writeln!(f, "gcsr_dmw2: {:016x}", self.gcsr_dmw2)?;
        writeln!(f, "gcsr_dmw3: {:016x}", self.gcsr_dmw3)?;
        writeln!(f, "host_estat: {:016x}", self.host_estat)?;
        writeln!(f, "host_era: {:016x}", self.host_era)?;
        writeln!(f, "host_badv: {:016x}", self.host_badv)?;
        writeln!(f, "host_badi: {:016x}", self.host_badi)?;
        writeln!(f, "host_tlbrbadv: {:016x}", self.host_tlbrbadv)?;
        write!(f, "host_tlbrera: {:016x}", self.host_tlbrera)
    }
}
