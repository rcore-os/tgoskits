use super::entry::_head;
use crate::consts::VM_LOAD_ADDRESS;

const R_RISCV_RELATIVE: u32 = 3;

pub fn apply() {
    let load_offset = get_load_offset();
    unsafe {
        crate::elf::apply_reloc(
            load_offset,
            ext_sym_addr!(__rela_dyn_begin) as _,
            ext_sym_addr!(__rela_dyn_end) as _,
            R_RISCV_RELATIVE,
        );
        ax_cpu::cache::flush_icache_all();
    }
}

pub fn reset() {
    unsafe {
        crate::elf::reset(R_RISCV_RELATIVE);
        ax_cpu::cache::flush_icache_all();
    }
}

fn get_load_offset() -> i128 {
    sym_addr!(_head) as i128 - VM_LOAD_ADDRESS as i128
}
