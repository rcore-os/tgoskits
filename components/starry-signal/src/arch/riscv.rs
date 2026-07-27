use ax_cpu::{GeneralRegisters, uspace::UserContext};

use crate::{SignalSet, SignalStack};

core::arch::global_asm!(
    "
.section .text
.balign 4096
.global signal_trampoline
signal_trampoline:
    li a7, 139
    ecall

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct MContext {
    gregs: SignalGeneralRegisters,
    fpstate: [usize; 66],
}

// SAFETY: both fields are contiguous integer storage, and their combined size
// is already a multiple of the declared 16-byte alignment.
unsafe impl bytemuck::NoUninit for MContext {}

impl MContext {
    pub fn new(uctx: &UserContext) -> Self {
        Self {
            gregs: SignalGeneralRegisters::new(uctx),
            fpstate: [0; 66],
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        self.gregs.restore(uctx);
    }
}

/// Linux RISC-V `user_regs_struct`: PC followed by architectural x1 through
/// x31. The kernel's internal [`GeneralRegisters`] also stores x0, so it cannot
/// be embedded directly without shifting the public signal ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct SignalGeneralRegisters {
    pc: usize,
    ra: usize,
    sp: usize,
    gp: usize,
    tp: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    s0: usize,
    s1: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    s2: usize,
    s3: usize,
    s4: usize,
    s5: usize,
    s6: usize,
    s7: usize,
    s8: usize,
    s9: usize,
    s10: usize,
    s11: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
}

impl SignalGeneralRegisters {
    fn new(uctx: &UserContext) -> Self {
        let regs = &uctx.regs;
        Self {
            pc: uctx.sepc,
            ra: regs.ra,
            sp: regs.sp,
            gp: regs.gp,
            tp: regs.tp,
            t0: regs.t0,
            t1: regs.t1,
            t2: regs.t2,
            s0: regs.s0,
            s1: regs.s1,
            a0: regs.a0,
            a1: regs.a1,
            a2: regs.a2,
            a3: regs.a3,
            a4: regs.a4,
            a5: regs.a5,
            a6: regs.a6,
            a7: regs.a7,
            s2: regs.s2,
            s3: regs.s3,
            s4: regs.s4,
            s5: regs.s5,
            s6: regs.s6,
            s7: regs.s7,
            s8: regs.s8,
            s9: regs.s9,
            s10: regs.s10,
            s11: regs.s11,
            t3: regs.t3,
            t4: regs.t4,
            t5: regs.t5,
            t6: regs.t6,
        }
    }

    fn restore(&self, uctx: &mut UserContext) {
        uctx.sepc = self.pc;
        uctx.regs = GeneralRegisters {
            zero: 0,
            ra: self.ra,
            sp: self.sp,
            gp: self.gp,
            tp: self.tp,
            t0: self.t0,
            t1: self.t1,
            t2: self.t2,
            s0: self.s0,
            s1: self.s1,
            a0: self.a0,
            a1: self.a1,
            a2: self.a2,
            a3: self.a3,
            a4: self.a4,
            a5: self.a5,
            a6: self.a6,
            a7: self.a7,
            s2: self.s2,
            s3: self.s3,
            s4: self.s4,
            s5: self.s5,
            s6: self.s6,
            s7: self.s7,
            s8: self.s8,
            s9: self.s9,
            s10: self.s10,
            s11: self.s11,
            t3: self.t3,
            t4: self.t4,
            t5: self.t5,
            t6: self.t6,
        };
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: SignalSet,
    __unused: [u8; 1024 / 8 - size_of::<SignalSet>()],
    __mcontext_align: [u8; 8],
    pub mcontext: MContext,
}

// SAFETY: every field implements `NoUninit`; the explicit alignment field
// consumes the gap before `MContext`.
unsafe impl bytemuck::NoUninit for UContext {}

impl UContext {
    pub fn new(uctx: &UserContext, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            sigmask,
            __unused: [0; 1024 / 8 - size_of::<SignalSet>()],
            __mcontext_align: [0; 8],
            mcontext: MContext::new(uctx),
        }
    }
}

const _: () = {
    assert!(size_of::<SignalGeneralRegisters>() == 32 * size_of::<usize>());
    assert!(core::mem::offset_of!(MContext, fpstate) == 32 * size_of::<usize>());
    assert!(size_of::<MContext>() == 98 * size_of::<usize>());
    assert!(core::mem::offset_of!(UContext, mcontext) == 176);
    assert!(size_of::<UContext>() == 960);
};
