//! LoongArch64 页表管理模块
//!
//! 参考 Linux kernel arch/loongarch/mm/tlb.c 和 arch/loongarch/include/asm/loongarch.h
//! 实现页表寄存器初始化和相关数据类型定义。

use num_align::NumAlign;
use page_table_generic::{MapConfig, TableMeta, VirtAddr};

// 导入 tock-registers 风格的页表项
pub use super::pte::Entry;
use crate::{
    arch::addrspace::to_phys,
    console::print_mapping,
    consts::PAGE_SIZE,
    mem::{__kimage_va, __va, MB, MemAttributes, PageTableInfo, PteConfig},
    smp::PerCpuMeta,
};

/// 页内偏移位数
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;

/// 每个页表索引的位数 = PAGE_SHIFT - 3 (页表项为8字节)
pub const PTE_INDEX_BITS: usize = PAGE_SHIFT - 3;

/// Invalidates the local boot translation state through the CPU owner.
#[inline(always)]
pub fn local_flush_tlb_all() {
    ax_cpu::mmu::flush_tlb(None);
}

/// Invalidates the current ASID's even/odd pair, including global mappings.
#[inline(always)]
pub fn local_flush_tlb_page(vaddr: usize) {
    ax_cpu::mmu::flush_tlb(Some(vaddr.into()));
}

#[cfg_attr(axtest_coverage, coverage(off))]
fn setup(root_paddr: usize) {
    assert_eq!(root_paddr & (PAGE_SIZE - 1), 0);
    // SAFETY: the boot owner retains its complete 4-KiB tree and executing mappings.
    unsafe { ax_cpu::boot::install_boot_page_table(root_paddr.into()) };
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
            Some(va) => local_flush_tlb_page(va.as_usize()),
            None => local_flush_tlb_all(),
        }
    }
}

pub fn relocate_kernel_to_vm_code() -> ! {
    let k_start = crate::mem::kimage_range().start;
    let mut table = crate::mem::mmu::new_boot_table();

    let pte = PteConfig {
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
            vaddr: VirtAddr::from_usize(v_start as usize),
            paddr: k_start.into(),
            size,
            pte,
            allow_huge: true,
            flush: false,
        })
        .unwrap();

    let tb_addr = table.root_paddr();
    crate::mem::mmu::set_boot_table(table);

    println!(
        "Boot page table at physical address: {:#x}",
        tb_addr.as_usize()
    );

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

    println!("Enabling MMU...");
    // 配置页大小并启用 MMU
    setup(tb.addr);

    println!("MMU enabled, jumping to {v_entry:#x}, sp={v_sp:#x}");

    // 在跳转到虚拟地址之前完成重定位重置
    // 这样可以避免修改正在执行的代码导致的指令缓存不一致问题
    crate::arch::relocate::reset();

    ax_cpu::cache::flush_icache_all();
    // SAFETY: relocation completed and the owner retained the mapped entry/stack.
    unsafe { ax_cpu::boot::jump_to(0, v_sp.into(), v_entry.into()) }
}

#[cfg_attr(axtest_coverage, coverage(off))]
pub fn enable_mmu_secondary(cpu_meta_paddr: usize) -> ! {
    let meta = unsafe {
        let phys_mask = (1usize << super::addrspace::PABITS) - 1;
        let meta_va = (cpu_meta_paddr & phys_mask) | super::addrspace::CACHE_BASE;
        &*(meta_va as *const PerCpuMeta)
    };
    setup(meta.boot_table_paddr);
    super::trap::init_entries_for_secondary();

    // SAFETY: setup installed the retained boot tree and secondary vectors;
    // metadata remains live through the final stack and instruction transfer.
    unsafe {
        ax_cpu::boot::enable_paged_translation();
        ax_cpu::boot::jump_to(
            cpu_meta_paddr,
            meta.stack_top_virt.into(),
            meta.entry_virt.into(),
        )
    }
}
