use axhal::arch::TrapFrame;

use crate::{SignalSet, SignalStack};

core::arch::global_asm!(
    "
.section .text
.code64
.balign 4096
.global signal_trampoline
signal_trampoline:
    mov rax, 0xf
    syscall

.fill 4096 - (. - signal_trampoline), 1, 0
"
);

#[repr(C)]
#[derive(Clone)]
pub struct MContext {
    r8: usize,
    r9: usize,
    r10: usize,
    r11: usize,
    r12: usize,
    r13: usize,
    r14: usize,
    r15: usize,
    rdi: usize,
    rsi: usize,
    rbp: usize,
    rbx: usize,
    rdx: usize,
    rax: usize,
    rcx: usize,
    rsp: usize,
    rip: usize,
    eflags: usize,
    cs: u16,
    gs: u16,
    fs: u16,
    _pad: u16,
    err: usize,
    trapno: usize,
    oldmask: usize,
    cr2: usize,
    fpstate: usize,
    _reserved1: [usize; 8],
}

impl MContext {
    pub fn new(tf: &TrapFrame) -> Self {
        Self {
            r8: tf.r8 as _,
            r9: tf.r9 as _,
            r10: tf.r10 as _,
            r11: tf.r11 as _,
            r12: tf.r12 as _,
            r13: tf.r13 as _,
            r14: tf.r14 as _,
            r15: tf.r15 as _,
            rdi: tf.rdi as _,
            rsi: tf.rsi as _,
            rbp: tf.rbp as _,
            rbx: tf.rbx as _,
            rdx: tf.rdx as _,
            rax: tf.rax as _,
            rcx: tf.rcx as _,
            rsp: tf.rsp as _,
            rip: tf.rip as _,
            eflags: tf.rflags as _,
            cs: tf.cs as _,
            gs: 0,
            fs: 0,
            _pad: 0,
            err: tf.error_code as _,
            trapno: tf.vector as _,
            oldmask: 0,
            cr2: 0,
            fpstate: 0,
            _reserved1: [0; 8],
        }
    }

    pub fn restore(&self, tf: &mut TrapFrame) {
        tf.r8 = self.r8 as _;
        tf.r9 = self.r9 as _;
        tf.r10 = self.r10 as _;
        tf.r11 = self.r11 as _;
        tf.r12 = self.r12 as _;
        tf.r13 = self.r13 as _;
        tf.r14 = self.r14 as _;
        tf.r15 = self.r15 as _;
        tf.rdi = self.rdi as _;
        tf.rsi = self.rsi as _;
        tf.rbp = self.rbp as _;
        tf.rbx = self.rbx as _;
        tf.rdx = self.rdx as _;
        tf.rax = self.rax as _;
        tf.rcx = self.rcx as _;
        tf.rsp = self.rsp as _;
        tf.rip = self.rip as _;
        tf.rflags = self.eflags as _;
        tf.cs = self.cs as _;
        tf.error_code = self.err as _;
        tf.vector = self.trapno as _;
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct UContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub mcontext: MContext,
    pub sigmask: SignalSet,
}

impl UContext {
    pub fn new(tf: &TrapFrame, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            mcontext: MContext::new(tf),
            sigmask,
        }
    }
}
