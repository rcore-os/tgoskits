use axhal::arch::{GeneralRegisters, TrapFrame};
use linux_raw_sys::general::SS_DISABLE;

use crate::ctypes::SignalSet;

#[repr(C)]
#[derive(Clone)]
pub struct SignalStack {
    pub sp: usize,
    pub flags: u32,
    pub size: usize,
}

impl Default for SignalStack {
    fn default() -> Self {
        Self {
            sp: 0,
            flags: SS_DISABLE,
            size: 0,
        }
    }
}

#[repr(C, align(16))]
struct MContextPadding([u8; 4096]);

#[repr(C)]
#[derive(Clone)]
pub struct MContext {
    sc_pc: GeneralRegisters,
    sc_regs: [u64; 32],
    sc_flags: u32,
}

impl MContext {
    pub fn new(tf: &TrapFrame) -> Self {
        Self {
            sc_pc: tf.era,
            sc_regs: tf.regs,
            sc_flags: 0,
        }
    }

    pub fn restore(&self, tf: &mut TrapFrame) {
        tf.era = self.sc_pc;
        tf.regs = self.sc_regs;
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
