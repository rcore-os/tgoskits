use crate::iocsr::{EIOINTC_ISR_BASE, EIOINTC_ISR_REG_COUNT};

pub(crate) fn ack_host_timer_interrupt() {
    unsafe {
        let value = crate::registers::guest_ticlr_clear_timer_value();
        core::arch::asm!("csrwr {}, 0x44", inout(reg) value => _);
    }
}

pub(crate) fn host_cpucfg(cpucfg_idx: usize) -> usize {
    let result: usize;
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) result, in(reg) cpucfg_idx);
    }
    result
}

pub(crate) fn host_eiointc_has_pending() -> bool {
    for reg in 0..EIOINTC_ISR_REG_COUNT {
        let addr = EIOINTC_ISR_BASE + reg * 8;
        let value: usize;
        unsafe {
            core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) addr);
        }
        if value != 0 {
            return true;
        }
    }

    false
}

pub(crate) fn host_iocsr_read_b(addr: usize) -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("iocsrrd.b {}, {}", out(reg) value, in(reg) addr);
    }
    value
}

pub(crate) fn host_iocsr_read_h(addr: usize) -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("iocsrrd.h {}, {}", out(reg) value, in(reg) addr);
    }
    value
}

pub(crate) fn host_iocsr_read_w(addr: usize) -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("iocsrrd.w {}, {}", out(reg) value, in(reg) addr);
    }
    value
}

pub(crate) fn host_iocsr_read_d(addr: usize) -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("iocsrrd.d {}, {}", out(reg) value, in(reg) addr);
    }
    value
}

pub(crate) fn host_iocsr_write_b(addr: usize, value: usize) {
    unsafe {
        core::arch::asm!("iocsrwr.b {}, {}", in(reg) value, in(reg) addr);
    }
}

pub(crate) fn host_iocsr_write_h(addr: usize, value: usize) {
    unsafe {
        core::arch::asm!("iocsrwr.h {}, {}", in(reg) value, in(reg) addr);
    }
}

pub(crate) fn host_iocsr_write_w(addr: usize, value: usize) {
    unsafe {
        core::arch::asm!("iocsrwr.w {}, {}", in(reg) value, in(reg) addr);
    }
}

pub(crate) fn host_iocsr_write_d(addr: usize, value: usize) {
    unsafe {
        core::arch::asm!("iocsrwr.d {}, {}", in(reg) value, in(reg) addr);
    }
}
