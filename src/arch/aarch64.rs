use axhal::arch::TrapFrame;

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
#[derive(Clone)]
struct MContextPadding([u8; 4096]);

#[repr(C)]
#[derive(Clone)]
pub struct MContext {
    fault_address: u64,
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
    __reserved: MContextPadding,
}

impl MContext {
    pub fn new(tf: &TrapFrame) -> Self {
        Self {
            fault_address: 0,
            regs: tf.r,
            sp: tf.usp,
            pc: tf.elr,
            pstate: tf.spsr,
            __reserved: MContextPadding([0; 4096]),
        }
    }

    pub fn restore(&self, tf: &mut TrapFrame) {
        tf.r = self.regs;
        tf.usp = self.sp;
        tf.elr = self.pc;
        tf.spsr = self.pstate;
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
    pub mcontext: MContext,
}

impl UContext {
    pub fn new(tf: &TrapFrame, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            sigmask,
            __unused: [0; 1024 / 8 - size_of::<SignalSet>()],
            mcontext: MContext::new(tf),
        }
    }
}
