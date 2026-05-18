//! LoongArch64 页表管理模块
//!
//! 参考 Linux kernel arch/loongarch/mm/tlb.c 和 arch/loongarch/include/asm/loongarch.h
//! 实现页表寄存器初始化和相关数据类型定义。

use core::arch::naked_asm;

use loongArch64::register::{pgdh, pgdl, pwch::*, pwcl::*, stlbps};
use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PteConfig, TableMeta, VirtAddr};

// 导入 tock-registers 风格的页表项
pub use super::pte::Entry;
use crate::{
    arch::addrspace::to_phys,
    console::print_mapping,
    consts::PAGE_SIZE,
    mem::{__kimage_va, __va, MB, PageTableInfo},
};

/// 4KB 页大小的 PS 值
#[cfg(page_size_4k)]
const PS: usize = 0x0c;
/// 16KB 页大小的 PS 值
#[cfg(page_size_16k)]
const PS: u64 = 0x0e;

/// 页内偏移位数
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;

// ============================================================================
// 页表层级配置
// ============================================================================

/// 每个页表索引的位数 = PAGE_SHIFT - 3 (页表项为8字节)
pub const PTE_INDEX_BITS: usize = PAGE_SHIFT - 3;

/// 无效化所有 TLB 条目
#[inline(always)]
pub fn local_flush_tlb_all() {
    unsafe {
        // invtlb op=0x0 (无效化所有 TLB)
        core::arch::asm!("dbar 0; invtlb 0x00, $r0, $r0", options(nomem, nostack));
    }
}

/// 无效化指定虚拟地址的 TLB 条目
#[inline(always)]
pub fn local_flush_tlb_page(vaddr: usize) {
    unsafe {
        // invtlb op=0x5 (按地址无效化, 不考虑 ASID)
        core::arch::asm!(
            "invtlb 0x5, $zero, {}",
            in(reg) vaddr,
            options(nomem, nostack)
        );
    }
}

// /// 无效化指定 ASID 的所有 TLB 条目
// #[inline(always)]
// pub fn local_flush_tlb_asid(asid: u64) {
//     unsafe {
//         // invtlb op=0x4 (按 ASID 无效化)
//         core::arch::asm!(
//             "invtlb 0x4, {}, $zero",
//             in(reg) asid,
//             options(nomem, nostack)
//         );
//     }
// }

// /// 无效化指定 ASID 和虚拟地址的 TLB 条目
// #[inline(always)]
// pub fn local_flush_tlb_page_asid(vaddr: usize, asid: u64) {
//     unsafe {
//         // invtlb op=0x6 (按地址和 ASID 无效化)
//         core::arch::asm!(
//             "invtlb 0x6, {}, {}",
//             in(reg) asid,
//             in(reg) vaddr,
//             options(nomem, nostack)
//         );
//     }
// }

/// 简化的页表初始化 (仅设置页大小和遍历器)
pub fn setup() {
    stlbps::set_ps(PS);

    set_dir3_base(12 + 9 + 9 + 9);
    set_dir3_width(9);
    set_dir2_base(12 + 9 + 9);
    set_dir2_width(9);
    set_dir1_base(12 + 9);
    set_dir1_width(9);
    set_ptbase(12);
    set_ptwidth(9);
    set_pte_width(8); // 64 bits -> 8 bytes

    local_flush_tlb_all();
}

// ============================================================================
// 页表泛型实现
// ============================================================================

/// LoongArch64 页表泛型配置
#[derive(Clone, Copy)]
pub struct Generic;

#[cfg(page_size_4k)]
impl TableMeta for Generic {
    type P = Entry;

    /// 页面大小
    const PAGE_SIZE: usize = 0x1000; // 4KB

    /// 各级索引位数数组 (从最高级到最低级: PGD -> PUD -> PMD -> PTE)
    /// 对于 4KB 页: 每级 9 位
    const LEVEL_BITS: &[usize] = &[
        PTE_INDEX_BITS, // Level 3 (PGD)
        PTE_INDEX_BITS, // Level 2 (PUD)
        PTE_INDEX_BITS, // Level 1 (PMD)
        PTE_INDEX_BITS, // Level 0 (PTE)
    ];

    /// 大页最高支持级别 (PMD 级别，即 Level 1)
    const MAX_BLOCK_LEVEL: usize = 1;

    /// 刷新 TLB
    fn flush(vaddr: Option<VirtAddr>) {
        match vaddr {
            Some(va) => local_flush_tlb_page(va.raw()),
            None => local_flush_tlb_all(),
        }
    }
}

pub fn relocate_kernel_to_vm_code() -> ! {
    let k_start = crate::mem::kimage_range().start;
    let mut table = crate::mem::mmu::new_boot_table();

    let pte = PteConfig {
        valid: true,
        read: true,
        writable: true,
        executable: true,
        mem_attr: MemAttributes::Normal,
        ..Default::default()
    };

    println!("Page table entry flags: {:?}", pte);

    let v_start = __kimage_va(k_start);
    let v_end = v_start as usize + crate::mem::kimage_range().len();
    let size = v_end.align_up(2 * MB) - v_start as usize;

    print_mapping("KImage", v_start as _, k_start, size);
    println!(
        "Mapping: vaddr={:#x}, paddr={:#x}, size={:#x}",
        v_start as usize, k_start, size
    );

    table
        .map(&MapConfig {
            vaddr: v_start.into(),
            paddr: k_start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })
        .unwrap();

    let tb_addr = table.root_paddr();
    crate::mem::mmu::set_boot_table(table);

    println!("Boot page table at physical address: {:#x}", tb_addr.raw());

    // Use physical address to avoid virtual address mapping issues
    let mmu_entry_phys = to_phys(super::entry::mmu_entry as *const () as usize);
    println!("MMU Entry point at physical address: {:#x}", mmu_entry_phys);

    let v_entry = __kimage_va(mmu_entry_phys) as usize;
    println!("MMU Entry virtual address: {:#x}", v_entry);

    let tb = PageTableInfo {
        asid: 0,
        addr: tb_addr.into(),
    };

    let v_sp = __va(to_phys(sym_running_addr!(__cpu0_stack_top))) as usize;
    let v_entry = __kimage_va(mmu_entry_phys) as usize;

    println!("Setting up page table...");

    pgdh::set_base(tb.addr as _);
    pgdl::set_base(tb.addr as _);

    // 添加数据同步屏障，确保页表写入完成
    unsafe {
        core::arch::asm!("dbar 0", options(nomem, nostack));
    }

    println!("Enabling MMU...");
    // 配置页大小并启用 MMU
    setup();

    println!("MMU enabled, jumping to {v_entry:#x}, sp={v_sp:#x}");

    // 在跳转到虚拟地址之前完成重定位重置
    // 这样可以避免修改正在执行的代码导致的指令缓存不一致问题
    crate::arch::relocate::reset();

    // 刷新指令缓存，确保跳转后执行的是正确位置的指令
    unsafe {
        core::arch::asm!("ibar 0", options(nomem, nostack));
        core::arch::asm!("dbar 0", options(nomem, nostack));
    }

    relocate_kernel(v_entry, v_sp);
    unreachable!()
}

#[unsafe(naked)]
extern "C" fn relocate_kernel(entry: usize, sp: usize) {
    naked_asm!(
        "
        move $sp, $a1
        jr $a0
        ",
    )
}
