//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use aarch64_cpu::registers::{ESR_EL1, FAR_EL1, Readable};
use ax_memory_addr::VirtAddr;
use tock_registers::LocalRegisterCopy;

use super::trap::{TrapKind, is_valid_page_fault};
pub use crate::uspace_common::{ExceptionKind, ExceptionSyndrome, ReturnReason};
use crate::{TrapFrame, trap::PageFaultFlags};

/// Context to enter user space.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct UserContext {
    tf: TrapFrame,
    /// Stack Pointer (SP_EL0).
    pub sp: u64,
    /// Software Thread ID Register (TPIDR_EL0).
    pub tpidr: u64,
}

impl UserContext {
    /// Creates a new user context with the given entry point, stack top, and argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        use aarch64_cpu::registers::SPSR_EL1;
        let mut regs = [0; 31];
        regs[0] = arg0 as _;
        Self {
            tf: TrapFrame {
                x: regs,
                elr: entry as _,
                spsr: (SPSR_EL1::M::EL0t
                    + SPSR_EL1::D::Masked
                    + SPSR_EL1::A::Masked
                    + SPSR_EL1::I::Unmasked
                    + SPSR_EL1::F::Masked)
                    .value,
                sp: 0,
            },
            sp: ustack_top.as_usize() as _,
            tpidr: 0,
        }
    }

    /// Emulates an EL0 `MRS Xt, ID_AA64*_EL1` that trapped as an "unknown"
    /// exception (EC=0), mirroring Linux's `emulate_mrs`. AArch64 CPU-feature
    /// detection — e.g. the Go runtime's cpu probing — reads these ID feature
    /// registers from EL0; reading an EL1 register from EL0 is UNDEFINED and
    /// traps, which would otherwise be delivered as SIGILL and crash every such
    /// program. Reads the real register (the kernel runs at EL1) into `Xt`,
    /// advances the PC, and returns `true`; returns `false` if the faulting
    /// instruction is not one of the emulated ID-register reads so the caller can
    /// fall back to SIGILL.
    ///
    /// # Safety
    /// Reads the 4-byte instruction at the trapped PC (`elr`) from the active
    /// user address space. The caller invokes this only while that aspace is
    /// installed and the PC was just executing, so the page is mapped.
    pub unsafe fn emulate_mrs_id_reg(&mut self) -> bool {
        let insn = unsafe { core::ptr::read(self.tf.elr as *const u32) };
        // MRS (register, read direction): bits[31:20] == 0xD53 and L (bit 21) set.
        if (insn & 0xFFF0_0000) != 0xD530_0000 || (insn & (1 << 21)) == 0 {
            return false;
        }
        let op0 = (insn >> 19) & 0x3;
        let op1 = (insn >> 16) & 0x7;
        let crn = (insn >> 12) & 0xf;
        let crm = (insn >> 8) & 0xf;
        let op2 = (insn >> 5) & 0x7;
        let rt = (insn & 0x1f) as usize;
        // The AArch64 ID feature register space is op0=3, op1=0, CRn=0, CRm=4..7.
        // We must NOT leak the raw EL1 register to EL0 (as Linux's `emulate_mrs`
        // also avoids): the host/QEMU CPU may advertise SVE/SME/MTE/BTI/PAuth/RAS
        // etc. that need kernel context-save/enable flows StarryOS does not have.
        // A program reading those bits would then execute the corresponding
        // instructions and crash or corrupt state. So we expose a sanitized
        // user-safe view: keep only the feature bits whose instructions are plain
        // and stateless (so they Just Work, including under TCG), and report
        // everything else as not-implemented (RAZ).
        macro_rules! rd {
            ($reg:literal) => {{
                let v: u64;
                unsafe { core::arch::asm!(concat!("mrs {}, ", $reg), out(reg) v) };
                v
            }};
        }
        // Field masks (each ID field is 4 bits):
        //   PFR0 low 24 bits = EL0/EL1/EL2/EL3/FP/AdvSIMD — baseline, always safe.
        //     Bits >=24 (GIC/RAS/SVE/SEL2/MPAM/AMU) are hidden.
        const PFR0_SAFE: u64 = 0x0000_0000_00FF_FFFF;
        //   ISAR1 PAuth fields APA[7:4] API[11:8] GPA[27:24] GPI[31:28] need kernel
        //   key management; clear them, keep DPB/JSCVT/FCMA/LRCPC/SB/BF16/I8MM/...
        const ISAR1_PAUTH: u64 = 0x0000_0000_FF00_0FF0;
        let val: u64 = match (op0, op1, crn, crm, op2) {
            (3, 0, 0, 4, 0) => rd!("ID_AA64PFR0_EL1") & PFR0_SAFE,
            (3, 0, 0, 6, 0) => rd!("ID_AA64ISAR0_EL1"),
            (3, 0, 0, 6, 1) => rd!("ID_AA64ISAR1_EL1") & !ISAR1_PAUTH,
            (3, 0, 0, 7, 0) => rd!("ID_AA64MMFR0_EL1"),
            (3, 0, 0, 7, 1) => rd!("ID_AA64MMFR1_EL1"),
            (3, 0, 0, 7, 2) => rd!("ID_AA64MMFR2_EL1"),
            // Every other ID register in the architectural space — PFR1/PFR2,
            // DFR0/1/2, ZFR0 (SVE), SMFR0 (SME), ISAR2/3 (PAuth/MOPS), MMFR3/4,
            // reserved — describes state-bearing or kernel-only features StarryOS
            // does not implement. Report not-implemented (RAZ) rather than SIGILL,
            // so feature probing degrades to the baseline path instead of crashing.
            (3, 0, 0, 4..=7, _) => 0,
            _ => return false,
        };
        // Rt == 31 encodes XZR; the result is discarded.
        if rt < 31 {
            self.tf.x[rt] = val;
        }
        self.tf.elr += 4;
        true
    }

    /// Normalizes a cloned user context so it can safely return to EL0.
    pub fn prepare_clone_child_return_state(&mut self) {
        use aarch64_cpu::registers::SPSR_EL1;

        self.tf.spsr = (self.tf.spsr
            & !(SPSR_EL1::M.mask
                | SPSR_EL1::D.mask
                | SPSR_EL1::A.mask
                | SPSR_EL1::I.mask
                | SPSR_EL1::F.mask))
            | (SPSR_EL1::M::EL0t
                + SPSR_EL1::D::Masked
                + SPSR_EL1::A::Masked
                + SPSR_EL1::I::Unmasked
                + SPSR_EL1::F::Masked)
                .value;
    }

    /// Clears any architecture single-step state after a debug exception.
    ///
    /// AArch64 user single-step is currently emulated by the Starry ptrace layer,
    /// so there is no saved CPU flag to clear here.
    pub const fn clear_single_step_after_debug(&mut self) -> bool {
        false
    }

    /// Returns the syscall instruction length in bytes.
    pub const fn syscall_insn_len(&self) -> usize {
        4
    }

    /// Gets the stack pointer.
    pub const fn sp(&self) -> usize {
        self.sp as _
    }

    /// Sets the stack pointer.
    pub const fn set_sp(&mut self, sp: usize) {
        self.sp = sp as _;
    }

    /// Gets the TLS area.
    pub const fn tls(&self) -> usize {
        self.tpidr as _
    }

    /// Sets the TLS area.
    pub const fn set_tls(&mut self, tls: usize) {
        self.tpidr = tls as _;
    }

    /// Enters user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `elr`).
    ///
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext) -> TrapKind;
        }

        crate::asm::disable_irqs();
        let kind = unsafe { enter_user(self) };

        let ret = match kind {
            TrapKind::Irq => {
                // See the EL1 site in trap.rs: publish the interrupted user frame
                // so a PMU overflow handler running inside `dispatch_irq` can
                // unwind the user call stack for `PERF_SAMPLE_CALLCHAIN`. The user
                // `x29` lives in `self.tf.x[29]`; `SP_EL0` still holds the user SP.
                // IRQs stay masked from `enter_user`'s trap until `enable_irqs`
                // below, so no nested IRQ observes a stale frame.
                unsafe { super::pmu::set_trap_frame(&self.tf as *const _) };
                crate::trap::dispatch_irq(0);
                super::pmu::clear_trap_frame();
                ReturnReason::Interrupt
            }
            TrapKind::Fiq | TrapKind::SError => ReturnReason::Unknown,
            TrapKind::Synchronous => {
                let esr = ESR_EL1.extract();
                let far = FAR_EL1.get() as usize;

                let iss = esr.read(ESR_EL1::ISS);

                match esr.read_as_enum(ESR_EL1::EC) {
                    Some(ESR_EL1::EC::Value::SVC64) => ReturnReason::Syscall,
                    Some(ESR_EL1::EC::Value::InstrAbortLowerEL) if is_valid_page_fault(iss) => {
                        ReturnReason::PageFault(
                            va!(far),
                            PageFaultFlags::EXECUTE | PageFaultFlags::USER,
                        )
                    }
                    Some(ESR_EL1::EC::Value::DataAbortLowerEL) if is_valid_page_fault(iss) => {
                        let wnr = (iss & (1 << 6)) != 0; // WnR: Write not Read
                        let cm = (iss & (1 << 8)) != 0; // CM: Cache maintenance
                        ReturnReason::PageFault(
                            va!(far),
                            if wnr & !cm {
                                PageFaultFlags::WRITE
                            } else {
                                PageFaultFlags::READ
                            } | PageFaultFlags::USER,
                        )
                    }
                    _ => ReturnReason::Exception(ExceptionInfo { esr, far }),
                }
            }
        };

        crate::asm::enable_irqs();
        ret
    }
}

impl Deref for UserContext {
    type Target = TrapFrame;

    fn deref(&self) -> &Self::Target {
        &self.tf
    }
}

impl DerefMut for UserContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tf
    }
}

/// Information about an exception that occurred in user space.
#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {
    /// Exception Syndrome Register
    pub esr: LocalRegisterCopy<u64, ESR_EL1::Register>,
    /// Fault Address Register
    pub far: usize,
}

impl ExceptionInfo {
    /// Returns the faulting virtual address when the CPU records one.
    pub const fn fault_addr(&self) -> Option<usize> {
        Some(self.far)
    }

    /// Returns architecture-neutral syndrome information for this exception.
    pub fn syndrome(&self) -> ExceptionSyndrome {
        ExceptionSyndrome {
            raw: self.esr_value(),
            class: self.ec_value(),
            iss: self.iss_value(),
        }
    }

    /// Returns the raw Exception Syndrome Register value.
    pub fn esr_value(&self) -> u64 {
        self.esr.get()
    }

    /// Returns the raw exception class bits.
    pub fn ec_value(&self) -> u64 {
        self.esr.read(ESR_EL1::EC)
    }

    /// Returns the instruction specific syndrome bits.
    pub fn iss_value(&self) -> u64 {
        self.esr.read(ESR_EL1::ISS)
    }

    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        match self.esr.read_as_enum(ESR_EL1::EC) {
            Some(ESR_EL1::EC::Value::Brk64) | Some(ESR_EL1::EC::Value::Bkpt32) => {
                ExceptionKind::Breakpoint
            }
            Some(ESR_EL1::EC::Value::IllegalExecutionState) | Some(ESR_EL1::EC::Value::Unknown) => {
                ExceptionKind::IllegalInstruction
            }
            Some(ESR_EL1::EC::Value::PCAlignmentFault)
            | Some(ESR_EL1::EC::Value::SPAlignmentFault) => ExceptionKind::Misaligned,
            _ => ExceptionKind::Other,
        }
    }
}
