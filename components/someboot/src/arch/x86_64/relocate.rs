#![allow(dead_code)]

use super::{addrspace::KERNEL_BASE, head::_head};

const R_X86_64_RELATIVE: u32 = 8;

pub fn relocate() {
    relocate_with_offset(get_load_offset());
}

pub fn reset() {
    unsafe {
        crate::elf::reset(R_X86_64_RELATIVE);
    }
}

fn get_load_offset() -> i128 {
    sym_addr!(_head) as i128 - KERNEL_BASE as i128
}

fn relocate_with_offset(offset: i128) {
    unsafe extern "C" {
        fn __rela_dyn_begin();
        fn __rela_dyn_end();
    }

    unsafe {
        crate::elf::apply_reloc(
            offset,
            sym_addr!(__rela_dyn_begin) as _,
            sym_addr!(__rela_dyn_end) as _,
            R_X86_64_RELATIVE,
        );
    }
}
