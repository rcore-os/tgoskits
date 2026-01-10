#![allow(dead_code)]

use core::arch::asm;

use super::addrspace::VM_LOAD_ADDRESS;

const R_LARCH_RELATIVE: u32 = 3;

macro_rules! sym_lma {
    ($sym:expr) => {{
        #[allow(unused_unsafe)]
        unsafe{
            let out: usize;
            core::arch::asm!(
                "la.pcrel    {r}, {s}",
                r = out(reg) out,
                s = sym $sym,
            );
            out
        }
    }};
}

unsafe extern "C" {
    fn _head();
    fn __rela_dyn_begin();
    fn __rela_dyn_end();
}

/// 计算加载偏移量 (实际地址 - 链接地址)
pub fn get_load_offset() -> i128 {
    sym_lma!(_head) as i128 - VM_LOAD_ADDRESS as i128
}

/// 早期重定位入口点
pub fn relocate() {
    relocate_with_offset(get_load_offset());
}

pub fn relocate_kernel_to_vm_code() {
    relocate_with_offset(0);
}

pub fn relocate_with_offset(offset: i128) {
    unsafe {
        crate::elf::apply_reloc(
            offset,
            sym_lma!(__rela_dyn_begin) as _,
            sym_lma!(__rela_dyn_end) as _,
            R_LARCH_RELATIVE,
        );
    }

    // 刷新指令与数据缓存，确保重定位后的数据立即生效
    unsafe {
        asm!("ibar 0", options(nostack));
        asm!("dbar 0", options(nostack));
    }
}
