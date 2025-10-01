use axcpu::uspace::UserContext;

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
    pub fn new(uctx: &UserContext) -> Self {
        Self {
            r8: uctx.r8 as _,
            r9: uctx.r9 as _,
            r10: uctx.r10 as _,
            r11: uctx.r11 as _,
            r12: uctx.r12 as _,
            r13: uctx.r13 as _,
            r14: uctx.r14 as _,
            r15: uctx.r15 as _,
            rdi: uctx.rdi as _,
            rsi: uctx.rsi as _,
            rbp: uctx.rbp as _,
            rbx: uctx.rbx as _,
            rdx: uctx.rdx as _,
            rax: uctx.rax as _,
            rcx: uctx.rcx as _,
            rsp: uctx.rsp as _,
            rip: uctx.rip as _,
            eflags: uctx.rflags as _,
            cs: uctx.cs as _,
            gs: 0,
            fs: 0,
            _pad: 0,
            err: uctx.error_code as _,
            trapno: uctx.vector as _,
            oldmask: 0,
            cr2: 0,
            fpstate: 0,
            _reserved1: [0; 8],
        }
    }

    pub fn restore(&self, uctx: &mut UserContext) {
        uctx.r8 = self.r8 as _;
        uctx.r9 = self.r9 as _;
        uctx.r10 = self.r10 as _;
        uctx.r11 = self.r11 as _;
        uctx.r12 = self.r12 as _;
        uctx.r13 = self.r13 as _;
        uctx.r14 = self.r14 as _;
        uctx.r15 = self.r15 as _;
        uctx.rdi = self.rdi as _;
        uctx.rsi = self.rsi as _;
        uctx.rbp = self.rbp as _;
        uctx.rbx = self.rbx as _;
        uctx.rdx = self.rdx as _;
        uctx.rax = self.rax as _;
        uctx.rcx = self.rcx as _;
        uctx.rsp = self.rsp as _;
        uctx.rip = self.rip as _;
        uctx.rflags = self.eflags as _;
        uctx.cs = self.cs as _;
        uctx.error_code = self.err as _;
        uctx.vector = self.trapno as _;
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
    pub fn new(uctx: &UserContext, sigmask: SignalSet) -> Self {
        Self {
            flags: 0,
            link: 0,
            stack: SignalStack::default(),
            mcontext: MContext::new(uctx),
            sigmask,
        }
    }
}
