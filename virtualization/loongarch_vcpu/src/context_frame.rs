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
#[repr(align(16))]
#[derive(Clone, Copy, Debug, Default)]
pub struct LoongArchContextFrame {
    pub x: [usize; 32],
    pub sepc: usize,
    pub gcsr_crmd: usize,
    pub gcsr_prmd: usize,
    pub gcsr_euen: usize,
    pub gcsr_misc: usize,
    pub gcsr_ectl: usize,
    pub gcsr_estat: usize,
    pub gcsr_era: usize,
    pub gcsr_badv: usize,
    pub gcsr_badi: usize,
    pub gcsr_eentry: usize,
    pub gcsr_tlbidx: usize,
    pub gcsr_tlbehi: usize,
    pub gcsr_tlbelo0: usize,
    pub gcsr_tlbelo1: usize,
    pub gcsr_asid: usize,
    pub gcsr_pgdl: usize,
    pub gcsr_pgdh: usize,
    pub gcsr_pgd: usize,
    pub gcsr_pwcl: usize,
    pub gcsr_pwch: usize,
    pub gcsr_stlbps: usize,
    pub gcsr_ravcfg: usize,
    pub gcsr_cpuid: usize,
    pub gcsr_prcfg1: usize,
    pub gcsr_prcfg2: usize,
    pub gcsr_prcfg3: usize,
    pub gcsr_save0: usize,
    pub gcsr_save1: usize,
    pub gcsr_save2: usize,
    pub gcsr_save3: usize,
    pub gcsr_save4: usize,
    pub gcsr_save5: usize,
    pub gcsr_save6: usize,
    pub gcsr_save7: usize,
    pub gcsr_save8: usize,
    pub gcsr_save9: usize,
    pub gcsr_save10: usize,
    pub gcsr_save11: usize,
    pub gcsr_save12: usize,
    pub gcsr_save13: usize,
    pub gcsr_save14: usize,
    pub gcsr_save15: usize,
    pub gcsr_tid: usize,
    pub gcsr_tcfg: usize,
    pub gcsr_tval: usize,
    pub gcsr_cntc: usize,
    pub gcsr_ticlr: usize,
    pub gcsr_llbctl: usize,
    pub gcsr_tlbrentry: usize,
    pub gcsr_tlbrbadv: usize,
    pub gcsr_tlbrera: usize,
    pub gcsr_tlbrsave: usize,
    pub gcsr_tlbrelo0: usize,
    pub gcsr_tlbrelo1: usize,
    pub gcsr_tlbrehi: usize,
    pub gcsr_tlbrprmd: usize,
    pub gcsr_dmw0: usize,
    pub gcsr_dmw1: usize,
    pub gcsr_dmw2: usize,
    pub gcsr_dmw3: usize,
    pub host_estat: usize,
    pub host_era: usize,
    pub host_badv: usize,
    pub host_badi: usize,
    pub host_tlbrbadv: usize,
    pub host_tlbrera: usize,
    pub guest_pc: usize,
    pub entry_host_era: usize,
    pub entry_guest_era: usize,
    pub entry_gstat: usize,
}

impl LoongArchContextFrame {
    pub fn set_argument(&mut self, arg: usize) {
        self.x[4] = arg;
    }

    pub fn set_a1(&mut self, val: usize) {
        self.x[5] = val;
    }

    pub fn set_a2(&mut self, val: usize) {
        self.x[6] = val;
    }

    pub fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0 => {}
            1..=31 => self.x[index] = val,
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
        writeln!(f, "host_tlbrera: {:016x}", self.host_tlbrera)?;
        writeln!(f, "guest_pc: {:016x}", self.guest_pc)?;
        writeln!(f, "entry_host_era: {:016x}", self.entry_host_era)?;
        writeln!(f, "entry_guest_era: {:016x}", self.entry_guest_era)?;
        write!(f, "entry_gstat: {:016x}", self.entry_gstat)
    }
}
