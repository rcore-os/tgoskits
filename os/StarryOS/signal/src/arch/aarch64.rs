use ax_cpu::uspace::UserContext;

use crate::{SignalSet, SignalStack};

core::arch::global_asm!(
    "
.section .text
.balign 4096
.global signal_trampoline
signal_trampoline:
    mov x8, #139
    svc #0

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct MContextPadding([u8; 4096]);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MContext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
    __reserved_align: [u8; 8],
    __reserved: MContextPadding,
}

// SAFETY: every byte is covered by integer arrays, including the explicit
// alignment gap before the 16-byte-aligned reserved area.
unsafe impl bytemuck::NoUninit for MContext {}

impl MContext {
    pub fn new(uctx: &UserContext) -> Self {
        Self {
            fault_address: 0,
            regs: uctx.x,
            sp: uctx.sp,
            pc: uctx.elr,
            pstate: uctx.spsr,
            __reserved_align: [0; 8],
            __reserved: MContextPadding([0; 4096]),
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        uctx.x = self.regs;
        uctx.sp = self.sp;
        uctx.elr = self.pc;
        uctx.spsr = self.pstate;
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
    assert!(core::mem::offset_of!(MContext, __reserved) == 288);
    assert!(size_of::<MContext>() == 4384);
    assert!(core::mem::offset_of!(UContext, mcontext) == 176);
    assert!(size_of::<UContext>() == 4560);
};
