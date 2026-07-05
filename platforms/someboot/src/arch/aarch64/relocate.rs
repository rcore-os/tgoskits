use aarch64_cpu_ext::cache::icache_flush_all;

use crate::{arch::head::_head, consts::VM_LOAD_ADDRESS};

// AArch64 重定位类型常量
const R_AARCH64_RELATIVE: u32 = 1027;

/// 计算加载偏移量 (实际地址 - 链接地址)
fn get_load_offset() -> i128 {
    sym_addr!(_head) as i128 - VM_LOAD_ADDRESS as i128
}

static mut OFFSET: i128 = 0;

/// 应用 .rela.dyn 重定位
pub fn apply() {
    unsafe {
        OFFSET = get_load_offset();
        crate::elf::apply_reloc(
            OFFSET,
            ext_sym_addr!(__rela_dyn_begin) as _,
            ext_sym_addr!(__rela_dyn_end) as _,
            R_AARCH64_RELATIVE,
        );
    }
}

pub fn reset() {
    unsafe {
        crate::elf::reset(R_AARCH64_RELATIVE);
        icache_flush_all();
    }
}

// pub(crate) fn print_reloc_info() {
//     unsafe {
//         let rela_start = ext_sym_addr!(__rela_dyn_begin);
//         let rela_end = ext_sym_addr!(__rela_dyn_end);
//         let rela_size = rela_end - rela_start;
//         let rela_count = rela_size / core::mem::size_of::<crate::elf::Rela>();
//         println!(
//             "Relocation entries from {:#x} to {:#x}, count: {}",
//             rela_start, rela_end, rela_count
//         );

//         let rela_slice =
//             core::slice::from_raw_parts(rela_start as *const crate::elf::Rela, rela_count);

//         for (i, rela) in rela_slice.iter().enumerate() {
//             println!(
//                 "Reloc[{}]: offset={:#x}, val={:#x}, addend={:#x}",
//                 i,
//                 rela.r_offset,
//                 (rela.r_offset as usize as *const u64).read(),
//                 rela.r_addend
//             );
//         }
//     }
// }
