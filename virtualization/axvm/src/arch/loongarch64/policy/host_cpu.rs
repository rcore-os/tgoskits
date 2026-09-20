use super::iocsr::{EIOINTC_ISR_BASE, EIOINTC_ISR_REG_COUNT};

pub(crate) fn host_cpucfg(index: usize) -> usize {
    ax_cpu::capability::read_cpucfg(index)
}

pub(crate) fn host_eiointc_has_pending() -> bool {
    (0..EIOINTC_ISR_REG_COUNT).any(|reg| host_iocsr_read_d(EIOINTC_ISR_BASE + reg * 8) != 0)
}

pub(crate) fn host_iocsr_read_b(address: usize) -> usize {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::read_iocsr8(address) as usize }
}

pub(crate) fn host_iocsr_write_b(address: usize, value: usize) {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::write_iocsr8(address, value as u8) }
}

pub(crate) fn host_iocsr_read_h(address: usize) -> usize {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::read_iocsr16(address) as usize }
}

pub(crate) fn host_iocsr_write_h(address: usize, value: usize) {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::write_iocsr16(address, value as u16) }
}

pub(crate) fn host_iocsr_read_w(address: usize) -> usize {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::read_iocsr32(address) as usize }
}

pub(crate) fn host_iocsr_write_w(address: usize, value: usize) {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::write_iocsr32(address, value as u32) }
}

pub(crate) fn host_iocsr_read_d(address: usize) -> usize {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::read_iocsr64(address) as usize }
}

pub(crate) fn host_iocsr_write_d(address: usize, value: usize) {
    // SAFETY: this is the VM platform's explicit unhandled-IOCSR passthrough path.
    unsafe { ax_cpu::registers::write_iocsr64(address, value as u64) }
}
