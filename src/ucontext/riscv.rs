use axhal::arch::{GeneralRegisters, TrapFrame};

use crate::ctypes::{SignalSet, SignalStack};

#[repr(C, align(16))]
#[derive(Clone)]
pub struct MContext {
    pub pc: usize,
    regs: GeneralRegisters,
    fpstate: [usize; 66],
}

impl MContext {
    pub fn new(tf: &TrapFrame) -> Self {
        Self {
            pc: tf.sepc,
            regs: tf.regs,
            fpstate: [0; 66],
        }
    }

    pub fn restore(&self, tf: &mut TrapFrame) {
        tf.sepc = self.pc;
        tf.regs = self.regs;
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
