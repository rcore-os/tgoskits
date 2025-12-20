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
