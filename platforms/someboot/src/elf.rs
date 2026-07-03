// RELA 重定位结构 (参考 include/uapi/linux/elf.h)
#[repr(C)]
pub struct Rela {
    pub r_offset: u64, // 需要重定位的地址
    pub r_info: u64,   // 类型和符号索引
    pub r_addend: i64, // 加数值
}

impl Rela {
    #[inline]
    fn r_type_raw(&self) -> u32 {
        (self.r_info & 0xFFFFFFFF) as u32
    }
}

/// 应用 .rela.dyn 重定位
/// # Safety
/// 此函数操作裸指针，调用者必须确保传入的指针范围有效且指向合法的 RELA 重定位表。
pub unsafe fn apply_reloc(load_offset: i128, start: *mut u8, end: *const u8, r_type: u32) {
    let num_entries = (end as usize - start as usize) / size_of::<Rela>();
    let relocations = unsafe { core::slice::from_raw_parts_mut(start as *mut Rela, num_entries) };

    for reloc in relocations {
        if reloc.r_type_raw() == r_type {
            let addr = (reloc.r_offset as i128 + load_offset) as usize as *mut usize;
            let val = (reloc.r_addend as i128 + load_offset) as usize;
            unsafe { *addr = val };
        }
    }
}

/// 应用 .rela.dyn 重定位
/// # Safety
/// 此函数操作裸指针，调用者必须确保传入的指针范围有效且指向合法的 RELA 重定位表。
pub unsafe fn reset(r_type: u32) {
    unsafe extern "C" {
        fn __rela_dyn_begin();
        fn __rela_dyn_end();
    }
    let start = __rela_dyn_begin as *mut u8;
    let end = __rela_dyn_end as *const u8;

    let num_entries = (end as usize - start as usize) / size_of::<Rela>();
    let relocations = unsafe { core::slice::from_raw_parts_mut(start as *mut Rela, num_entries) };
    for reloc in relocations {
        if reloc.r_type_raw() == r_type {
            let addr = reloc.r_offset as usize as *mut usize;
            unsafe { addr.write_volatile(reloc.r_addend as u64 as usize) };
        }
    }
}
