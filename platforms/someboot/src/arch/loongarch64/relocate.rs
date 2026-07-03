#![allow(dead_code)]

use core::arch::asm;

use super::addrspace::VM_LOAD_ADDRESS;

const R_LARCH_RELATIVE: u32 = 3;

pub fn _head_lma() -> usize {
    sym_running_addr!(_head)
}

/// 计算加载偏移量 (实际地址 - 链接地址)
pub fn get_load_offset() -> i128 {
    sym_running_addr!(_head) as i128 - VM_LOAD_ADDRESS as i128
}

/// 早期重定位入口点
pub fn relocate() {
    relocate_with_offset(get_load_offset());
}

pub fn reset() {
    unsafe {
        crate::elf::reset(R_LARCH_RELATIVE);
    }
}

pub fn relocate_with_offset(offset: i128) {
    unsafe {
        crate::elf::apply_reloc(
            offset,
            sym_running_addr!(__rela_dyn_begin) as _,
            sym_running_addr!(__rela_dyn_end) as _,
            R_LARCH_RELATIVE,
        );
    }

    // 刷新指令与数据缓存，确保重定位后的数据立即生效
    unsafe {
        asm!("ibar 0", options(nostack));
        asm!("dbar 0", options(nostack));
    }
}
