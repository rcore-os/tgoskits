use crate::ArchTrait;
#[allow(unused_imports)]
pub use crate::arch::irq::*;

pub fn irq_local_is_enabled() -> bool {
    crate::arch::Arch::irq_all_is_enabled()
}

pub fn irq_local_set_enable(enabled: bool) {
    crate::arch::Arch::irq_all_set_enable(enabled);
}

pub fn irq_is_enabled(irq: IrqId) -> bool {
    crate::arch::Arch::irq_is_enabled(irq)
}

pub fn irq_set_enable(irq: IrqId, enable: bool) {
    crate::arch::Arch::irq_set_enable(irq, enable);
}

/// 全局唯一的软件中断号，平台自行转换为本地硬件中断号或外部中断号
#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IrqId(usize);

impl IrqId {
    pub const fn new(id: usize) -> Self {
        IrqId(id)
    }

    pub const fn raw(&self) -> usize {
        self.0
    }
}

impl From<usize> for IrqId {
    fn from(value: usize) -> Self {
        IrqId(value)
    }
}

impl From<u32> for IrqId {
    fn from(value: u32) -> Self {
        IrqId(value as usize)
    }
}

#[allow(dead_code)]
pub(crate) fn handle_irq(hwirq: IrqId) {
    unsafe extern "Rust" {
        fn _someboot_handle_irq(hwirq: IrqId);
    }
    unsafe {
        _someboot_handle_irq(hwirq);
    }
}
