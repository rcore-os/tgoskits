use crate::arch::head::_head;
use crate::consts::VM_LOAD_ADDRESS;

// AArch64 重定位类型常量
const R_AARCH64_RELATIVE: u32 = 1027;

/// 计算加载偏移量 (实际地址 - 链接地址)
fn get_load_offset() -> i64 {
    sym_addr!(_head) as i64 - VM_LOAD_ADDRESS as i64
}

/// 应用 .rela.dyn 重定位
pub fn apply() {
    unsafe {
        crate::elf::apply_reloc(
            get_load_offset(),
            ext_sym_addr!(__rela_dyn_begin) as _,
            ext_sym_addr!(__rela_dyn_end) as _,
            R_AARCH64_RELATIVE,
        );
    }
}

pub(crate) fn print_reloc_info() {
    unsafe {
        let rela_start = ext_sym_addr!(__rela_dyn_begin);
        let rela_end = ext_sym_addr!(__rela_dyn_end);
        let rela_size = rela_end - rela_start;
        let rela_count = rela_size / core::mem::size_of::<crate::elf::Rela>();
        println!(
            "Relocation entries from {:#x} to {:#x}, count: {}",
            rela_start, rela_end, rela_count
        );

        let rela_slice = core::slice::from_raw_parts(
            rela_start as *const crate::elf::Rela,
            rela_count,
        );

        for (i, rela) in rela_slice.iter().enumerate() {
            println!(
                "Reloc[{}]: offset={:#x}, info={:#x}, addend={:#x}",
                i, rela.r_offset, rela.r_info, rela.r_addend
            );
        }
    }
}
