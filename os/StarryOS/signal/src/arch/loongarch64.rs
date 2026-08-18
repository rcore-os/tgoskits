use ax_cpu::{FpuState, GeneralRegisters, uspace::UserContext};

use crate::{SignalSet, SignalStack};

core::arch::global_asm!(
    "
.section .text
.balign 4096
.global signal_trampoline
signal_trampoline:
    li.w    $a7, 139
    syscall 0

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

#[repr(C, align(16))]
#[derive(Clone)]
pub struct MContext {
    sc_pc: u64,
    sc_regs: GeneralRegisters,
    sc_flags: u32,
    // Kernel-saved FP/FCC/FCSR of the interrupted thread. The musl-visible
    // mcontext fields a handler reads are sc_pc/sc_regs/sc_flags above; this FP
    // block lives where musl places `__extcontext` and is managed only by the
    // kernel. Traps (incl. the signal-delivery path) do not save FP, so without
    // this an async signal — e.g. a HotSpot safepoint-poll SIGSEGV interrupting
    // FP-using user code — would resume with the handler's clobbered FP and
    // corrupt state (gradle/kotlin compiler SIGSEGV on loongarch, #237).
    fpu: FpuState,
}

impl MContext {
    pub fn new(uctx: &UserContext) -> Self {
        // Capture the interrupted thread's live FP before the handler runs. The
        // kernel is softfloat and does not touch FP between the trap and here,
        // so the FP registers still hold the interrupted user state.
        let mut fpu = FpuState::default();
        fpu.save();
        Self {
            sc_pc: uctx.era as _,
            sc_regs: uctx.regs,
            sc_flags: 0,
            fpu,
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        uctx.era = self.sc_pc as _;
        uctx.regs = self.sc_regs;
        // Restore FP the handler may have clobbered, before resuming user code.
        self.fpu.restore();
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: SignalSet,
    __unused: [u8; 1024 / 8 - size_of::<SignalSet>()],
    // musl loongarch64 `ucontext_t` has `long __uc_pad` between `uc_sigmask` and
    // `uc_mcontext`; without it `mcontext` lands 8 bytes early and a userspace
    // SIGSEGV handler reading/writing `uc_mcontext` (e.g. HotSpot advancing the
    // saved PC / inspecting GPRs) corrupts the resumed register state.
    __uc_pad: u64,
    pub mcontext: MContext,
}

impl UContext {
    pub fn new(uctx: &UserContext, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            sigmask,
            __unused: [0; 1024 / 8 - size_of::<SignalSet>()],
            __uc_pad: 0,
            mcontext: MContext::new(uctx),
        }
    }
}
