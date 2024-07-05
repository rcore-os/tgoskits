use core::arch::asm;
use core::arch::global_asm;
use core::fmt::Formatter;
use core::arch::asm;
use cortex_a::registers::*;

use axhal::{msr, mrs};
use axhal::arch::TrapFrame;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Aarch64ContextFrame {
    pub gpr: [u64; 31],
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
}

impl core::fmt::Display for Aarch64ContextFrame {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), core::fmt::Error> {
        for i in 0..31 {
            write!(f, "x{:02}: {:016x}   ", i, self.gpr[i])?;
            if (i + 1) % 2 == 0 {
                write!(f, "\n")?;
            }
        }
        writeln!(f, "spsr:{:016x}", self.spsr)?;
        write!(f, "elr: {:016x}", self.elr)?;
        writeln!(f, "   sp:  {:016x}", self.sp)?;
        Ok(())
    }
}

impl From<Aarch64ContextFrame> for TrapFrame {
    fn from(item: Aarch64ContextFrame) -> Self {
        TrapFrame {
            r: item.gpr,
            usp: item.sp,
            elr: item.elr,
            spsr: item.spsr,
        }
    }
}

impl Aarch64ContextFrame {
    pub fn default() -> Aarch64ContextFrame {
        Aarch64ContextFrame {
            gpr: [0; 31],
            spsr: (SPSR_EL1::M::EL1h
                + SPSR_EL1::I::Masked
                + SPSR_EL1::F::Masked
                + SPSR_EL1::A::Masked
                + SPSR_EL1::D::Masked)
                .value as u64,
            elr: 0,
            sp: 0,
        }
    }

    fn new(pc: usize, sp: usize, arg: usize) -> Self {
        let mut r = Aarch64ContextFrame {
            gpr: [0; 31],
            spsr: (SPSR_EL1::M::EL1h
                + SPSR_EL1::I::Masked
                + SPSR_EL1::F::Masked
                + SPSR_EL1::A::Masked
                + SPSR_EL1::D::Masked)
                .value as u64,
            elr: pc as u64,
            sp: sp as u64,
        };
        r.set_argument(arg);
        r
    }

    fn exception_pc(&self) -> usize {
        self.elr as usize
    }

    fn set_exception_pc(&mut self, pc: usize) {
        self.elr = pc as u64;
    }

    fn stack_pointer(&self) -> usize {
        self.sp as usize
    }

    fn set_stack_pointer(&mut self, sp: usize) {
        self.sp = sp as u64;
    }

    fn set_argument(&mut self, arg: usize) {
        self.gpr[0] = arg as u64;
    }

    fn set_gpr(&mut self, index: usize, val: usize) {
        self.gpr[index] = val as u64;
    }

    fn gpr(&self, index: usize) -> usize {
        self.gpr[index] as usize
    }
}

#[repr(C)]
#[repr(align(16))]
#[derive(Debug, Clone)]
pub struct VmContext {
    // generic timer
    pub cntvoff_el2: u64,
    cntp_cval_el0: u64,
    cntv_cval_el0: u64,
    pub cntkctl_el1: u32,
    pub cntvct_el0: u64,
    cntp_ctl_el0: u32,
    cntv_ctl_el0: u32,
    cntp_tval_el0: u32,
    cntv_tval_el0: u32,

    // vpidr and vmpidr
    vpidr_el2: u32,
    pub vmpidr_el2: u64,

    // 64bit EL1/EL0 register
    sp_el0: u64,
    sp_el1: u64,
    elr_el1: u64,
    spsr_el1: u32,
    pub sctlr_el1: u32,
    actlr_el1: u64,
    cpacr_el1: u32,
    ttbr0_el1: u64,
    ttbr1_el1: u64,
    tcr_el1: u64,
    esr_el1: u32,
    far_el1: u64,
    par_el1: u64,
    mair_el1: u64,
    amair_el1: u64,
    vbar_el1: u64,
    contextidr_el1: u32,
    tpidr_el0: u64,
    tpidr_el1: u64,
    tpidrro_el0: u64,

    // hypervisor context
    pub hcr_el2: u64,
    cptr_el2: u64,
    hstr_el2: u64,
    pub pmcr_el0: u64,
    pub vtcr_el2: u64,

    // exception
    far_el2: u64,
    hpfar_el2: u64,
}

impl VmContext {
    pub fn default() -> VmContext {
        VmContext {
            // generic timer
            cntvoff_el2: 0,
            cntp_cval_el0: 0,
            cntv_cval_el0: 0,
            cntkctl_el1: 0,
            cntvct_el0: 0,
            cntp_ctl_el0: 0,
            cntv_ctl_el0: 0,
            cntp_tval_el0: 0,
            cntv_tval_el0: 0,

            // vpidr and vmpidr
            vpidr_el2: 0,
            vmpidr_el2: 0,

            // 64bit EL1/EL0 register
            sp_el0: 0,
            sp_el1: 0,
            elr_el1: 0,
            spsr_el1: 0,
            sctlr_el1: 0,
            actlr_el1: 0,
            cpacr_el1: 0,
            ttbr0_el1: 0,
            ttbr1_el1: 0,
            tcr_el1: 0,
            esr_el1: 0,
            far_el1: 0,
            par_el1: 0,
            mair_el1: 0,
            amair_el1: 0,
            vbar_el1: 0,
            contextidr_el1: 0,
            tpidr_el0: 0,
            tpidr_el1: 0,
            tpidrro_el0: 0,

            // hypervisor context
            hcr_el2: 0,
            cptr_el2: 0,
            hstr_el2: 0,

            // exception
            pmcr_el0: 0,
            vtcr_el2: 0,
            far_el2: 0,
            hpfar_el2: 0,
            gic_state: GicState::default(),
        }
    }

    pub fn reset(&mut self) {
        self.cntvoff_el2 = 0;
        self.cntp_cval_el0 = 0;
        self.cntv_cval_el0 = 0;
        self.cntp_tval_el0 = 0;
        self.cntv_tval_el0 = 0;
        self.cntkctl_el1 = 0;
        self.cntvct_el0 = 0;
        self.cntp_ctl_el0 = 0;
        self.vpidr_el2 = 0;
        self.vmpidr_el2 = 0;
        self.sp_el0 = 0;
        self.sp_el1 = 0;
        self.elr_el1 = 0;
        self.spsr_el1 = 0;
        self.sctlr_el1 = 0;
        self.actlr_el1 = 0;
        self.cpacr_el1 = 0;
        self.ttbr0_el1 = 0;
        self.ttbr1_el1 = 0;
        self.tcr_el1 = 0;
        self.esr_el1 = 0;
        self.far_el1 = 0;
        self.par_el1 = 0;
        self.mair_el1 = 0;
        self.amair_el1 = 0;
        self.vbar_el1 = 0;
        self.contextidr_el1 = 0;
        self.tpidr_el0 = 0;
        self.tpidr_el1 = 0;
        self.tpidrro_el0 = 0;
        self.hcr_el2 = 0;
        self.cptr_el2 = 0;
        self.hstr_el2 = 0;
        self.far_el2 = 0;
        self.hpfar_el2 = 0;
    }

    pub fn ext_regs_store(&mut self) {
        mrs!(self.cntvoff_el2, CNTVOFF_EL2);
        // MRS!(self.cntp_cval_el0, CNTP_CVAL_EL0);
        mrs!(self.cntv_cval_el0, CNTV_CVAL_EL0);
        mrs!(self.cntkctl_el1, CNTKCTL_EL1, "x");
        mrs!(self.cntp_ctl_el0, CNTP_CTL_EL0, "x");
        mrs!(self.cntv_ctl_el0, CNTV_CTL_EL0, "x");
        mrs!(self.cntp_tval_el0, CNTP_TVAL_EL0, "x");
        mrs!(self.cntv_tval_el0, CNTV_TVAL_EL0, "x");
        mrs!(self.cntvct_el0, CNTVCT_EL0);
        // MRS!("self.vpidr_el2, VPIDR_EL2, "x");
        mrs!(self.vmpidr_el2, VMPIDR_EL2);

        mrs!(self.sp_el0, SP_EL0);
        mrs!(self.sp_el1, SP_EL1);
        mrs!(self.elr_el1, ELR_EL1);
        mrs!(self.spsr_el1, SPSR_EL1, "x");
        mrs!(self.sctlr_el1, SCTLR_EL1, "x");
        mrs!(self.cpacr_el1, CPACR_EL1, "x");
        mrs!(self.ttbr0_el1, TTBR0_EL1);
        mrs!(self.ttbr1_el1, TTBR1_EL1);
        mrs!(self.tcr_el1, TCR_EL1);
        mrs!(self.esr_el1, ESR_EL1, "x");
        mrs!(self.far_el1, FAR_EL1);
        mrs!(self.par_el1, PAR_EL1);
        mrs!(self.mair_el1, MAIR_EL1);
        mrs!(self.amair_el1, AMAIR_EL1);
        mrs!(self.vbar_el1, VBAR_EL1);
        mrs!(self.contextidr_el1, CONTEXTIDR_EL1, "x");
        mrs!(self.tpidr_el0, TPIDR_EL0);
        mrs!(self.tpidr_el1, TPIDR_EL1);
        mrs!(self.tpidrro_el0, TPIDRRO_EL0);

        mrs!(self.pmcr_el0, PMCR_EL0);
        mrs!(self.vtcr_el2, VTCR_EL2);
        mrs!(self.hcr_el2, HCR_EL2);
        // MRS!(self.cptr_el2, CPTR_EL2);
        // MRS!(self.hstr_el2, HSTR_EL2);
        // MRS!(self.far_el2, FAR_EL2);
        // MRS!(self.hpfar_el2, HPFAR_EL2);
        mrs!(self.actlr_el1, ACTLR_EL1);
        // println!("save sctlr {:x}", self.sctlr_el1);
    }

    pub fn ext_regs_restore(&self) {
        // println!("restore CNTV_CTL_EL0 {:x}", self.cntv_ctl_el0);
        // println!("restore CNTV_CVAL_EL0 {:x}", self.cntv_cval_el0);
        // MSR!(CNTP_CVAL_EL0, self.cntp_cval_el0);
        msr!(CNTV_CVAL_EL0, self.cntv_cval_el0);
        msr!(CNTKCTL_EL1, self.cntkctl_el1, "x");
        // MSR!(CNTP_CTL_EL0, self.cntp_ctl_el0, "x");
        msr!(CNTV_CTL_EL0, self.cntv_ctl_el0, "x");
        // MSR!(CNTP_TVAL_EL0, {0:x}", in(reg) self.cntp_tval_el0, "x");
        // MSR!(CNTV_TVAL_EL0, {0:x}", in(reg) self.cntv_tval_el0, "x");

        msr!(SP_EL0, self.sp_el0);
        msr!(SP_EL1, self.sp_el1);
        msr!(ELR_EL1, self.elr_el1);
        msr!(SPSR_EL1, self.spsr_el1, "x");
        msr!(SCTLR_EL1, self.sctlr_el1, "x");
        msr!(CPACR_EL1, self.cpacr_el1, "x");
        msr!(TTBR0_EL1, self.ttbr0_el1);
        msr!(TTBR1_EL1, self.ttbr1_el1);
        msr!(TCR_EL1, self.tcr_el1);
        msr!(ESR_EL1, self.esr_el1, "x");
        msr!(FAR_EL1, self.far_el1);
        msr!(PAR_EL1, self.par_el1);
        msr!(MAIR_EL1, self.mair_el1);
        msr!(AMAIR_EL1, self.amair_el1);
        msr!(VBAR_EL1, self.vbar_el1);
        msr!(CONTEXTIDR_EL1, self.contextidr_el1, "x");
        msr!(TPIDR_EL0, self.tpidr_el0);
        msr!(TPIDR_EL1, self.tpidr_el1);
        msr!(TPIDRRO_EL0, self.tpidrro_el0);

        msr!(PMCR_EL0, self.pmcr_el0);
        // MSR!(CPTR_EL2, self.cptr_el2);
        // MSR!(HSTR_EL2, self.hstr_el2);
        // MSR!(FAR_EL2, self.far_el2);
        // MSR!(HPFAR_EL2, self.hpfar_el2);
        msr!(ACTLR_EL1, self.actlr_el1);

        msr!(VTCR_EL2, self.vtcr_el2);
        msr!(HCR_EL2, self.hcr_el2);
        // MSR!(VPIDR_EL2, self.vpidr_el2, "x");
        msr!(VMPIDR_EL2, self.vmpidr_el2);
        msr!(CNTVOFF_EL2, self.cntvoff_el2);
    }
}
