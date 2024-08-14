use aarch64_cpu::registers::*;
use core::arch::asm;
use core::arch::global_asm;
use core::fmt::Formatter;

use axhal::arch::TrapFrame;

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
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

impl From<TrapFrame> for Aarch64ContextFrame {
    fn from(item: TrapFrame) -> Self {
        Aarch64ContextFrame {
            gpr: item.r,
            sp: item.usp,
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

    pub fn new(pc: usize, sp: usize, arg: usize) -> Self {
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

    pub fn exception_pc(&self) -> usize {
        self.elr as usize
    }

    pub fn set_exception_pc(&mut self, pc: usize) {
        self.elr = pc as u64;
    }

    pub fn stack_pointer(&self) -> usize {
        self.sp as usize
    }

    pub fn set_stack_pointer(&mut self, sp: usize) {
        self.sp = sp as u64;
    }

    pub fn set_argument(&mut self, arg: usize) {
        self.gpr[0] = arg as u64;
    }

    pub fn set_gpr(&mut self, index: usize, val: usize) {
        self.gpr[index] = val as u64;
    }

    pub fn gpr(&self, index: usize) -> usize {
        self.gpr[index] as usize
    }
}

#[repr(C)]
#[repr(align(16))]
#[derive(Debug, Clone, Copy, Default)]
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
    pub vttbr_el2: u64,
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
            vttbr_el2: 0,
            far_el2: 0,
            hpfar_el2: 0,
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
        unsafe {
            asm!("mrs {0}, CNTVOFF_EL2", out(reg) self.cntvoff_el2);
            asm!("mrs {0}, CNTV_CVAL_EL0", out(reg) self.cntv_cval_el0);
            asm!("mrs {0:x}, CNTKCTL_EL1", out(reg) self.cntkctl_el1);
            asm!("mrs {0:x}, CNTP_CTL_EL0", out(reg) self.cntp_ctl_el0);
            asm!("mrs {0:x}, CNTV_CTL_EL0", out(reg) self.cntv_ctl_el0);
            asm!("mrs {0:x}, CNTP_TVAL_EL0", out(reg) self.cntp_tval_el0);
            asm!("mrs {0:x}, CNTV_TVAL_EL0", out(reg) self.cntv_tval_el0);
            asm!("mrs {0}, CNTVCT_EL0", out(reg) self.cntvct_el0);
            // MRS!("self.vpidr_el2, VPIDR_EL2, "x");
            asm!("mrs {0}, VMPIDR_EL2", out(reg) self.vmpidr_el2);

            asm!("mrs {0}, SP_EL0", out(reg) self.sp_el0);
            asm!("mrs {0}, SP_EL1", out(reg) self.sp_el1);
            asm!("mrs {0}, ELR_EL1", out(reg) self.elr_el1);
            asm!("mrs {0:x}, SPSR_EL1", out(reg) self.spsr_el1);
            asm!("mrs {0:x}, SCTLR_EL1", out(reg) self.sctlr_el1);
            asm!("mrs {0:x}, CPACR_EL1", out(reg) self.cpacr_el1);
            asm!("mrs {0}, TTBR0_EL1", out(reg) self.ttbr0_el1);
            asm!("mrs {0}, TTBR1_EL1", out(reg) self.ttbr1_el1);
            asm!("mrs {0}, TCR_EL1", out(reg) self.tcr_el1);
            asm!("mrs {0:x}, ESR_EL1", out(reg) self.esr_el1);
            asm!("mrs {0}, FAR_EL1", out(reg) self.far_el1);
            asm!("mrs {0}, PAR_EL1", out(reg) self.par_el1);
            asm!("mrs {0}, MAIR_EL1", out(reg) self.mair_el1);
            asm!("mrs {0}, AMAIR_EL1", out(reg) self.amair_el1);
            asm!("mrs {0}, VBAR_EL1", out(reg) self.vbar_el1);
            asm!("mrs {0:x}, CONTEXTIDR_EL1", out(reg) self.contextidr_el1);
            asm!("mrs {0}, TPIDR_EL0", out(reg) self.tpidr_el0);
            asm!("mrs {0}, TPIDR_EL1", out(reg) self.tpidr_el1);
            asm!("mrs {0}, TPIDRRO_EL0", out(reg) self.tpidrro_el0);

            asm!("mrs {0}, PMCR_EL0", out(reg) self.pmcr_el0);
            asm!("mrs {0}, VTCR_EL2", out(reg) self.vtcr_el2);
            asm!("mrs {0}, VTTBR_EL2", out(reg) self.vttbr_el2);
            asm!("mrs {0}, HCR_EL2", out(reg) self.hcr_el2);
            asm!("mrs {0}, ACTLR_EL1", out(reg) self.actlr_el1);
        }
        // println!("save sctlr {:x}", self.sctlr_el1);
    }

    pub fn ext_regs_restore(&self) {
        unsafe {
            asm!("msr CNTV_CVAL_EL0, {0}", in(reg) self.cntv_cval_el0);
            asm!("msr CNTKCTL_EL1, {0:x}", in (reg) self.cntkctl_el1);
            asm!("msr CNTV_CTL_EL0, {0:x}", in (reg) self.cntv_ctl_el0);
            asm!("msr SP_EL0, {0}", in(reg) self.sp_el0);
            asm!("msr SP_EL1, {0}", in(reg) self.sp_el1);
            asm!("msr ELR_EL1, {0}", in(reg) self.elr_el1);
            asm!("msr SPSR_EL1, {0:x}", in(reg) self.spsr_el1);
            asm!("msr SCTLR_EL1, {0:x}", in(reg) self.sctlr_el1);
            asm!("msr CPACR_EL1, {0:x}", in(reg) self.cpacr_el1);
            asm!("msr TTBR0_EL1, {0}", in(reg) self.ttbr0_el1);
            asm!("msr TTBR1_EL1, {0}", in(reg) self.ttbr1_el1);
            asm!("msr TCR_EL1, {0}", in(reg) self.tcr_el1);
            asm!("msr ESR_EL1, {0:x}", in(reg) self.esr_el1);
            asm!("msr FAR_EL1, {0}", in(reg) self.far_el1);
            asm!("msr PAR_EL1, {0}", in(reg) self.par_el1);
            asm!("msr MAIR_EL1, {0}", in(reg) self.mair_el1);
            asm!("msr AMAIR_EL1, {0}", in(reg) self.amair_el1);
            asm!("msr VBAR_EL1, {0}", in(reg) self.vbar_el1);
            asm!("msr CONTEXTIDR_EL1, {0:x}", in(reg) self.contextidr_el1);
            asm!("msr TPIDR_EL0, {0}", in(reg) self.tpidr_el0);
            asm!("msr TPIDR_EL1, {0}", in(reg) self.tpidr_el1);
            asm!("msr TPIDRRO_EL0, {0}", in(reg) self.tpidrro_el0);

            asm!("msr PMCR_EL0, {0}", in(reg) self.pmcr_el0);
            asm!("msr ACTLR_EL1, {0}", in(reg) self.actlr_el1);

            asm!("msr VTCR_EL2, {0}", in(reg) self.vtcr_el2);
            asm!("msr VTTBR_EL2, {0}", in(reg) self.vttbr_el2);
            asm!("msr HCR_EL2, {0}", in(reg) self.hcr_el2);
            asm!("msr VMPIDR_EL2, {0}", in(reg) self.vmpidr_el2);
            asm!("msr CNTVOFF_EL2, {0}", in(reg) self.cntvoff_el2);
        }
    }
}
