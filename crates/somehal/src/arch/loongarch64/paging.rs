//! LoongArch64 页表管理模块
//!
//! 参考 Linux kernel arch/loongarch/mm/tlb.c 和 arch/loongarch/include/asm/loongarch.h
//! 实现页表寄存器初始化和相关数据类型定义。

#![allow(dead_code)]

use core::arch::naked_asm;

use kernutil::StaticCell;
use loongArch64::register::{pgdh, pgdl, pwch::*, pwcl::*, stlbps};
use num_align::NumAlign;
use page_table_generic::{MapConfig, MemAttributes, PteConfig, TableGeneric, VirtAddr};

// 导入 tock-registers 风格的页表项
pub use super::pte::Entry;

use crate::{
    arch::addrspace::to_phys,
    console::print_mapping,
    consts::PAGE_SIZE,
    mem::{__kimage_va, __va, MB, PageTableInfo, ram::Ram},
};

static BOOT_TABLE: StaticCell<page_table_generic::PageTable<Generic, Ram>> = StaticCell::uninit();

// ============================================================================
// CSR 寄存器地址定义
// 参考: Linux arch/loongarch/include/asm/loongarch.h
// ============================================================================

/// ASID - 地址空间标识符寄存器
pub const LOONGARCH_CSR_ASID: u32 = 0x18;
/// 低位虚拟地址页表基地址 (VA[VALEN-1] = 0)
pub const LOONGARCH_CSR_PGDL: u32 = 0x19;
/// 高位虚拟地址页表基地址 (VA[VALEN-1] = 1)
pub const LOONGARCH_CSR_PGDH: u32 = 0x1a;
/// 页表基地址 (当前使用)
pub const LOONGARCH_CSR_PGD: u32 = 0x1b;
/// 页表遍历控制寄存器0
pub const LOONGARCH_CSR_PWCTL0: u32 = 0x1c;
/// 页表遍历控制寄存器1
pub const LOONGARCH_CSR_PWCTL1: u32 = 0x1d;
/// STLB 页大小寄存器
pub const LOONGARCH_CSR_STLBPGSIZE: u32 = 0x1e;
/// RVACFG 寄存器
pub const LOONGARCH_CSR_RVACFG: u32 = 0x1f;
/// TLB Index 寄存器
pub const LOONGARCH_CSR_TLBIDX: u32 = 0x10;
/// TLB EntryHi 寄存器
pub const LOONGARCH_CSR_TLBEHI: u32 = 0x11;
/// TLB EntryLo0 寄存器
pub const LOONGARCH_CSR_TLBELO0: u32 = 0x12;
/// TLB EntryLo1 寄存器
pub const LOONGARCH_CSR_TLBELO1: u32 = 0x13;
/// TLB Refill Entry High
pub const LOONGARCH_CSR_TLBREHI: u32 = 0x8e;

// ============================================================================
// PWCTL0 寄存器字段定义
// ============================================================================

/// PWCTL0.PTEW - 页表项宽度 (shift)
pub const CSR_PWCTL0_PTEW_SHIFT: u64 = 30;
/// PWCTL0.PTEW - 页表项宽度 (width)
pub const CSR_PWCTL0_PTEW_WIDTH: u64 = 2;
/// PWCTL0.DIR1WIDTH - 目录1宽度 (shift)
pub const CSR_PWCTL0_DIR1WIDTH_SHIFT: u64 = 25;
/// PWCTL0.DIR1BASE - 目录1基址 (shift)
pub const CSR_PWCTL0_DIR1BASE_SHIFT: u64 = 20;
/// PWCTL0.DIR0WIDTH - 目录0宽度 (shift)
pub const CSR_PWCTL0_DIR0WIDTH_SHIFT: u64 = 15;
/// PWCTL0.DIR0BASE - 目录0基址 (shift)
pub const CSR_PWCTL0_DIR0BASE_SHIFT: u64 = 10;
/// PWCTL0.PTWIDTH - 页表宽度 (shift)
pub const CSR_PWCTL0_PTWIDTH_SHIFT: u64 = 5;
/// PWCTL0.PTBASE - 页表基址 (shift)
pub const CSR_PWCTL0_PTBASE_SHIFT: u64 = 0;

// ============================================================================
// PWCTL1 寄存器字段定义
// ============================================================================

/// PWCTL1.PTW - 硬件页表遍历使能 (shift)
pub const CSR_PWCTL1_PTW_SHIFT: u64 = 24;
/// PWCTL1.PTW - 硬件页表遍历使能
pub const CSR_PWCTL1_PTW: u64 = 1 << CSR_PWCTL1_PTW_SHIFT;
/// PWCTL1.DIR3WIDTH - 目录3宽度 (shift)
pub const CSR_PWCTL1_DIR3WIDTH_SHIFT: u64 = 18;
/// PWCTL1.DIR3BASE - 目录3基址 (shift)
pub const CSR_PWCTL1_DIR3BASE_SHIFT: u64 = 12;
/// PWCTL1.DIR2WIDTH - 目录2宽度 (shift)
pub const CSR_PWCTL1_DIR2WIDTH_SHIFT: u64 = 6;
/// PWCTL1.DIR2BASE - 目录2基址 (shift)
pub const CSR_PWCTL1_DIR2BASE_SHIFT: u64 = 0;

// ============================================================================
// 页表项标志位定义
// 参考: Linux arch/loongarch/include/asm/pgtable-bits.h
// ============================================================================

/// 页有效位 (Valid)
pub const _PAGE_VALID_SHIFT: u64 = 0;
pub const _PAGE_VALID: u64 = 1 << _PAGE_VALID_SHIFT;

/// 页脏位 (Dirty)
pub const _PAGE_DIRTY_SHIFT: u64 = 1;
pub const _PAGE_DIRTY: u64 = 1 << _PAGE_DIRTY_SHIFT;

/// 特权级位 (PLV) - 2位
pub const _PAGE_PLV_SHIFT: u64 = 2;
pub const _PAGE_PLV_WIDTH: u64 = 2;
pub const _PAGE_PLV_MASK: u64 = ((1 << _PAGE_PLV_WIDTH) - 1) << _PAGE_PLV_SHIFT;

/// 缓存属性位 (Cache) - 2位
pub const _CACHE_SHIFT: u64 = 4;
pub const _CACHE_WIDTH: u64 = 2;
pub const _CACHE_MASK: u64 = ((1 << _CACHE_WIDTH) - 1) << _CACHE_SHIFT;

/// 全局位 (Global) - 用于PTE
pub const _PAGE_GLOBAL_SHIFT: u64 = 6;
pub const _PAGE_GLOBAL: u64 = 1 << _PAGE_GLOBAL_SHIFT;

/// 巨页位 (Huge) - 用于PMD
pub const _PAGE_HUGE_SHIFT: u64 = 6;
pub const _PAGE_HUGE: u64 = 1 << _PAGE_HUGE_SHIFT;

/// 存在位 (Present)
pub const _PAGE_PRESENT_SHIFT: u64 = 7;
pub const _PAGE_PRESENT: u64 = 1 << _PAGE_PRESENT_SHIFT;

/// 写位 (Write)
pub const _PAGE_WRITE_SHIFT: u64 = 8;
pub const _PAGE_WRITE: u64 = 1 << _PAGE_WRITE_SHIFT;

/// 修改位 (Modified)
pub const _PAGE_MODIFIED_SHIFT: u64 = 9;
pub const _PAGE_MODIFIED: u64 = 1 << _PAGE_MODIFIED_SHIFT;

/// PROTNONE 位
pub const _PAGE_PROTNONE_SHIFT: u64 = 10;
pub const _PAGE_PROTNONE: u64 = 1 << _PAGE_PROTNONE_SHIFT;

/// 特殊位 (Special)
pub const _PAGE_SPECIAL_SHIFT: u64 = 11;
pub const _PAGE_SPECIAL: u64 = 1 << _PAGE_SPECIAL_SHIFT;

/// 巨页全局位 (HGlobal) - 用于PMD
pub const _PAGE_HGLOBAL_SHIFT: u64 = 12;
pub const _PAGE_HGLOBAL: u64 = 1 << _PAGE_HGLOBAL_SHIFT;

/// 物理页帧号位移
pub const _PAGE_PFN_SHIFT: u64 = 12;
/// 物理页帧号宽度 (36位，支持最大48位物理地址)
pub const _PAGE_PFN_WIDTH: u64 = 36;
pub const _PAGE_PFN_MASK: u64 = ((1u64 << _PAGE_PFN_WIDTH) - 1) << _PAGE_PFN_SHIFT;

/// 禁止读位 (No Read)
pub const _PAGE_NO_READ_SHIFT: u64 = 61;
pub const _PAGE_NO_READ: u64 = 1 << _PAGE_NO_READ_SHIFT;

/// 禁止执行位 (No Execute)
pub const _PAGE_NO_EXEC_SHIFT: u64 = 62;
pub const _PAGE_NO_EXEC: u64 = 1 << _PAGE_NO_EXEC_SHIFT;

/// RPLV 位
pub const _PAGE_RPLV_SHIFT: u64 = 63;
pub const _PAGE_RPLV: u64 = 1 << _PAGE_RPLV_SHIFT;

// ============================================================================
// 缓存属性定义
// ============================================================================

/// 强序非缓存 (Strongly-ordered UnCached)
pub const CACHE_SUC: u64 = 0 << _CACHE_SHIFT;
/// 一致性缓存 (Coherent Cached)
pub const CACHE_CC: u64 = 1 << _CACHE_SHIFT;
/// 弱序非缓存 (Weakly-ordered UnCached)
pub const CACHE_WUC: u64 = 2 << _CACHE_SHIFT;

// ============================================================================
// 页面大小定义
// ============================================================================

/// 4KB 页大小的 PS 值
pub const PS_4K: usize = 0x0c;
/// 16KB 页大小的 PS 值
pub const PS_16K: u64 = 0x0e;
/// 64KB 页大小的 PS 值
pub const PS_64K: u64 = 0x10;
/// 2MB 巨页的 PS 值
pub const PS_2M: u64 = 0x15;
/// 1GB 巨页的 PS 值
pub const PS_1G: u64 = 0x1e;

/// 默认页大小 (4KB = 0x0c)
pub const PS_DEFAULT_SIZE: usize = PS_4K;

/// 页内偏移位数
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;

// ============================================================================
// 页表层级配置
// ============================================================================

/// 每个页表索引的位数 = PAGE_SHIFT - 3 (页表项为8字节)
pub const PTE_INDEX_BITS: usize = PAGE_SHIFT - 3;

/// 每个页表的条目数
pub const PTRS_PER_PTE: usize = 1 << PTE_INDEX_BITS;

// 4级页表配置 (以 4KB 页为例):
// - PTE: bits [12..21] = 9 bits, 512 entries
// - PMD: bits [21..30] = 9 bits, 512 entries
// - PUD: bits [30..39] = 9 bits, 512 entries
// - PGD: bits [39..48] = 9 bits, 512 entries

/// PMD 偏移 (4KB 页: 21)
pub const PMD_SHIFT: usize = PAGE_SHIFT + PTE_INDEX_BITS;
/// PUD 偏移 (4KB 页: 30)
pub const PUD_SHIFT: usize = PMD_SHIFT + PTE_INDEX_BITS;
/// PGD 偏移 (4KB 页: 39)
pub const PGDIR_SHIFT: usize = PUD_SHIFT + PTE_INDEX_BITS;

// ============================================================================
// TLBIDX 寄存器字段
// ============================================================================

/// TLBIDX.PS - 页大小字段偏移
pub const CSR_TLBIDX_PS_SHIFT: u32 = 24;
pub const CSR_TLBIDX_PS_WIDTH: u32 = 6;
pub const CSR_TLBIDX_PS_MASK: u64 = ((1 << CSR_TLBIDX_PS_WIDTH) - 1) << CSR_TLBIDX_PS_SHIFT;

/// TLBIDX.IDX - 索引字段偏移
pub const CSR_TLBIDX_IDX_SHIFT: u32 = 0;
pub const CSR_TLBIDX_IDX_WIDTH: u32 = 12;
pub const CSR_TLBIDX_IDX_MASK: u64 = (1 << CSR_TLBIDX_IDX_WIDTH) - 1;

// ============================================================================
// TLBREHI 寄存器字段
// ============================================================================

/// TLBREHI.PS - TLB Refill 页大小字段偏移
pub const CSR_TLBREHI_PS_SHIFT: u64 = 0;
pub const CSR_TLBREHI_PS_WIDTH: u64 = 6;
pub const CSR_TLBREHI_PS: u64 = ((1 << CSR_TLBREHI_PS_WIDTH) - 1) << CSR_TLBREHI_PS_SHIFT;

// ============================================================================
// 页表寄存器操作宏
// ============================================================================

/// 读取 CSR 寄存器（使用 csrrd 指令）
macro_rules! csr_read {
    ($csr:expr) => {{
        let value: u64;
        unsafe {
            core::arch::asm!(
                "csrrd {}, {}",
                out(reg) value,
                const $csr,
                options(nomem, nostack)
            );
        }
        value
    }};
}

/// 写入 CSR 寄存器（使用 csrwr 指令）
macro_rules! csr_write {
    ($csr:expr, $value:expr) => {{
        let val: u64 = $value;
        unsafe {
            core::arch::asm!(
                "csrwr {}, {}",
                in(reg) val,
                const $csr,
                options(nomem, nostack)
            );
        }
    }};
}

// ============================================================================
// 页表寄存器操作函数
// ============================================================================

/// 读取 ASID 寄存器
#[inline(always)]
pub fn read_csr_asid() -> u64 {
    csr_read!(LOONGARCH_CSR_ASID)
}

/// 写入 ASID 寄存器
#[inline(always)]
pub fn write_csr_asid(val: u64) {
    csr_write!(LOONGARCH_CSR_ASID, val);
}

/// 读取页大小
#[inline(always)]
pub fn read_csr_pagesize() -> u64 {
    (csr_read!(LOONGARCH_CSR_TLBIDX) & CSR_TLBIDX_PS_MASK) >> CSR_TLBIDX_PS_SHIFT
}

/// 写入页大小
#[inline(always)]
pub fn write_csr_pagesize(size: u64) {
    let old = csr_read!(LOONGARCH_CSR_TLBIDX);
    let new = (old & !CSR_TLBIDX_PS_MASK) | (size << CSR_TLBIDX_PS_SHIFT);
    csr_write!(LOONGARCH_CSR_TLBIDX, new);
}

/// 读取 STLB 页大小
#[inline(always)]
pub fn read_csr_stlbpgsize() -> u64 {
    csr_read!(LOONGARCH_CSR_STLBPGSIZE)
}

/// 写入 STLB 页大小
#[inline(always)]
pub fn write_csr_stlbpgsize(size: u64) {
    csr_write!(LOONGARCH_CSR_STLBPGSIZE, size);
}

/// 读取 TLB Refill 页大小
#[inline(always)]
pub fn read_csr_tlbrefill_pagesize() -> u64 {
    (csr_read!(LOONGARCH_CSR_TLBREHI) & CSR_TLBREHI_PS) >> CSR_TLBREHI_PS_SHIFT
}

/// 写入 TLB Refill 页大小
#[inline(always)]
pub fn write_csr_tlbrefill_pagesize(size: u64) {
    let old = csr_read!(LOONGARCH_CSR_TLBREHI);
    let new = (old & !CSR_TLBREHI_PS) | (size << CSR_TLBREHI_PS_SHIFT);
    csr_write!(LOONGARCH_CSR_TLBREHI, new);
}

/// 读取 PGDL (低地址空间页表基地址)
#[inline(always)]
pub fn read_csr_pgdl() -> u64 {
    csr_read!(LOONGARCH_CSR_PGDL)
}

/// 写入 PGDL
#[inline(always)]
pub fn write_csr_pgdl(val: u64) {
    csr_write!(LOONGARCH_CSR_PGDL, val);
}

/// 读取 PGDH (高地址空间页表基地址)
#[inline(always)]
pub fn read_csr_pgdh() -> u64 {
    csr_read!(LOONGARCH_CSR_PGDH)
}

/// 写入 PGDH
#[inline(always)]
pub fn write_csr_pgdh(val: u64) {
    csr_write!(LOONGARCH_CSR_PGDH, val);
}

/// 读取 PGD (当前页表基地址)
#[inline(always)]
pub fn read_csr_pgd() -> u64 {
    csr_read!(LOONGARCH_CSR_PGD)
}

/// 读取 PWCTL0
#[inline(always)]
pub fn read_csr_pwctl0() -> u64 {
    csr_read!(LOONGARCH_CSR_PWCTL0)
}

/// 写入 PWCTL0
#[inline(always)]
pub fn write_csr_pwctl0(val: u64) {
    csr_write!(LOONGARCH_CSR_PWCTL0, val);
}

/// 读取 PWCTL1
#[inline(always)]
pub fn read_csr_pwctl1() -> u64 {
    csr_read!(LOONGARCH_CSR_PWCTL1)
}

/// 写入 PWCTL1
#[inline(always)]
pub fn write_csr_pwctl1(val: u64) {
    csr_write!(LOONGARCH_CSR_PWCTL1, val);
}

// ============================================================================
// TLB 操作
// ============================================================================

/// TLB 搜索
#[inline(always)]
pub fn tlb_probe() {
    unsafe {
        core::arch::asm!("tlbsrch", options(nomem, nostack));
    }
}

/// TLB 读取
#[inline(always)]
pub fn tlb_read() {
    unsafe {
        core::arch::asm!("tlbrd", options(nomem, nostack));
    }
}

/// TLB 按索引写入
#[inline(always)]
pub fn tlb_write_indexed() {
    unsafe {
        core::arch::asm!("tlbwr", options(nomem, nostack));
    }
}

/// TLB 随机写入
#[inline(always)]
pub fn tlb_write_random() {
    unsafe {
        core::arch::asm!("tlbfill", options(nomem, nostack));
    }
}

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

/// 无效化指定 ASID 的所有 TLB 条目
#[inline(always)]
pub fn local_flush_tlb_asid(asid: u64) {
    unsafe {
        // invtlb op=0x4 (按 ASID 无效化)
        core::arch::asm!(
            "invtlb 0x4, {}, $zero",
            in(reg) asid,
            options(nomem, nostack)
        );
    }
}

/// 无效化指定 ASID 和虚拟地址的 TLB 条目
#[inline(always)]
pub fn local_flush_tlb_page_asid(vaddr: usize, asid: u64) {
    unsafe {
        // invtlb op=0x6 (按地址和 ASID 无效化)
        core::arch::asm!(
            "invtlb 0x6, {}, {}",
            in(reg) asid,
            in(reg) vaddr,
            options(nomem, nostack)
        );
    }
}

/// 简化的页表初始化 (仅设置页大小和遍历器)
pub fn setup() {
    #[cfg(page_size_4k)]
    const PS: usize = PS_4K;
    #[cfg(page_size_16k)]
    const PS: usize = PS_16K as usize;

    // tlbidx::set_ps(PS);
    stlbps::set_ps(PS);
    // tlbrehi::set_ps(PS);
    set_dir3_base(12 + 9 + 9 + 9);
    set_dir3_width(9);
    set_dir2_base(12 + 9 + 9);
    set_dir2_width(9);
    set_dir1_base(12 + 9);
    set_dir1_width(9);
    set_ptbase(12);
    set_ptwidth(9);
    set_pte_width(8); // 64 bits -> 8 bytes

    // // Enable mapped address translation mode
    // crmd::set_pg(true);
    local_flush_tlb_all();
}

// ============================================================================
// 页表泛型实现
// ============================================================================

/// LoongArch64 页表泛型配置
#[derive(Clone, Copy)]
pub struct Generic;

#[cfg(page_size_4k)]
impl TableGeneric for Generic {
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

// ============================================================================
// CPUCFG 相关定义 (用于检测 CPU 特性)
// ============================================================================

/// 检查是否支持硬件页表遍历 (PTW)
pub fn cpu_has_ptw() -> bool {
    // 读取 CPUCFG word 1
    let cfg1: u64;
    unsafe {
        core::arch::asm!(
            "cpucfg {}, {}",
            out(reg) cfg1,
            in(reg) 1u64,
            options(nomem, nostack)
        );
    }
    // bit 24 = PTW 支持
    (cfg1 & (1 << 24)) != 0
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

    let v_sp = __va(to_phys(ext_sym_addr!(__cpu0_stack_top))) as usize;
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
