//! LoongArch64 页表管理模块
//!
//! 参考 Linux kernel arch/loongarch/mm/tlb.c 和 arch/loongarch/include/asm/loongarch.h
//! 实现页表寄存器初始化和相关数据类型定义。

#![allow(dead_code)]

use core::arch::naked_asm;

use kernutil::StaticCell;
use num_align::NumAlign;
use page_table_generic::{MapConfig, MemConfig, PageTableEntry, PhysAddr, TableGeneric, VirtAddr};

use crate::{
    ArchTrait,
    arch::Arch,
    consts::PAGE_SIZE,
    mem::{PageTableInfo, page_size, ram::Ram, virt_to_phys},
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
pub const PS_4K: u64 = 0x0c;
/// 16KB 页大小的 PS 值
pub const PS_16K: u64 = 0x0e;
/// 64KB 页大小的 PS 值
pub const PS_64K: u64 = 0x10;
/// 2MB 巨页的 PS 值
pub const PS_2M: u64 = 0x15;
/// 1GB 巨页的 PS 值
pub const PS_1G: u64 = 0x1e;

/// 默认页大小 (4KB = 0x0c)
pub const PS_DEFAULT_SIZE: u64 = PS_4K;

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
        core::arch::asm!("invtlb 0, $zero, $zero", options(nomem, nostack));
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

// ============================================================================
// 页表遍历器配置
// 参考: Linux arch/loongarch/mm/tlb.c - setup_ptwalker()
// ============================================================================

/// PWCTL0 配置结构
#[derive(Debug, Clone, Copy)]
pub struct PwCtl0 {
    /// 页表基址 (PTE 偏移)
    pub pt_base: u64,
    /// 页表宽度
    pub pt_width: u64,
    /// 目录0基址 (PMD 偏移)
    pub dir0_base: u64,
    /// 目录0宽度
    pub dir0_width: u64,
    /// 目录1基址 (PUD 偏移)
    pub dir1_base: u64,
    /// 目录1宽度
    pub dir1_width: u64,
}

impl PwCtl0 {
    /// 创建默认配置
    pub const fn new() -> Self {
        Self {
            pt_base: PAGE_SHIFT as u64,
            pt_width: PTE_INDEX_BITS as u64,
            dir0_base: PMD_SHIFT as u64,
            dir0_width: PTE_INDEX_BITS as u64,
            dir1_base: PUD_SHIFT as u64,
            dir1_width: PTE_INDEX_BITS as u64,
        }
    }

    /// 编码为 CSR 值
    pub const fn encode(&self) -> u64 {
        self.pt_base
            | (self.pt_width << CSR_PWCTL0_PTWIDTH_SHIFT)
            | (self.dir0_base << CSR_PWCTL0_DIR0BASE_SHIFT)
            | (self.dir0_width << CSR_PWCTL0_DIR0WIDTH_SHIFT)
            | (self.dir1_base << CSR_PWCTL0_DIR1BASE_SHIFT)
            | (self.dir1_width << CSR_PWCTL0_DIR1WIDTH_SHIFT)
    }
}

impl Default for PwCtl0 {
    fn default() -> Self {
        Self::new()
    }
}

/// PWCTL1 配置结构
#[derive(Debug, Clone, Copy)]
pub struct PwCtl1 {
    /// 目录2基址 (PGD 偏移)
    pub dir2_base: u64,
    /// 目录2宽度
    pub dir2_width: u64,
    /// 目录3基址 (保留)
    pub dir3_base: u64,
    /// 目录3宽度
    pub dir3_width: u64,
    /// 是否启用硬件页表遍历
    pub ptw_enable: bool,
}

impl PwCtl1 {
    /// 创建默认配置
    pub const fn new() -> Self {
        Self {
            dir2_base: PGDIR_SHIFT as u64,
            dir2_width: PTE_INDEX_BITS as u64,
            dir3_base: 0,
            dir3_width: 0,
            ptw_enable: false,
        }
    }

    /// 编码为 CSR 值
    pub const fn encode(&self) -> u64 {
        let mut val = self.dir2_base
            | (self.dir2_width << CSR_PWCTL1_DIR2WIDTH_SHIFT)
            | (self.dir3_base << CSR_PWCTL1_DIR3BASE_SHIFT)
            | (self.dir3_width << CSR_PWCTL1_DIR3WIDTH_SHIFT);
        if self.ptw_enable {
            val |= CSR_PWCTL1_PTW;
        }
        val
    }
}

impl Default for PwCtl1 {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// 页表初始化
// ============================================================================

/// 设置页表遍历器
/// 参考: Linux arch/loongarch/mm/tlb.c - setup_ptwalker()
pub fn setup_ptwalker() {
    let pwctl0 = PwCtl0::new();
    let pwctl1 = PwCtl1::new();

    write_csr_pwctl0(pwctl0.encode());
    write_csr_pwctl1(pwctl1.encode());
}

/// 初始化页表相关寄存器
/// 参考: Linux arch/loongarch/mm/tlb.c - tlb_init()
pub fn setup_with_pg_dir(swapper_pg_dir: usize, invalid_pg_dir: usize) {
    // 设置页大小
    write_csr_pagesize(PS_DEFAULT_SIZE);
    write_csr_stlbpgsize(PS_DEFAULT_SIZE);
    write_csr_tlbrefill_pagesize(PS_DEFAULT_SIZE);

    // 设置页表遍历器
    setup_ptwalker();

    // 设置页表基地址
    // PGDH: 高地址空间 (内核空间)
    write_csr_pgdh(swapper_pg_dir as u64);
    // PGDL: 低地址空间 (用户空间，初始化为无效页表)
    write_csr_pgdl(invalid_pg_dir as u64);

    // 刷新 TLB
    local_flush_tlb_all();
}

/// 简化的页表初始化 (仅设置页大小和遍历器)
pub fn setup() {
    // 设置页大小
    write_csr_pagesize(PS_DEFAULT_SIZE);
    write_csr_stlbpgsize(PS_DEFAULT_SIZE);
    write_csr_tlbrefill_pagesize(PS_DEFAULT_SIZE);

    // 设置页表遍历器
    setup_ptwalker();
}

// ============================================================================
// 页表项类型
// ============================================================================

/// LoongArch64 页表项
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Entry(u64);

impl Entry {
    /// 创建空页表项
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 创建全局无效页表项
    pub const fn invalid_global() -> Self {
        Self(_PAGE_GLOBAL)
    }

    /// 获取原始值
    pub const fn raw(&self) -> u64 {
        self.0
    }

    /// 从原始值创建
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// 检查是否有效
    pub const fn is_valid(&self) -> bool {
        (self.0 & _PAGE_VALID) != 0
    }

    /// 检查是否存在
    pub const fn is_present(&self) -> bool {
        (self.0 & _PAGE_PRESENT) != 0
    }

    /// 检查是否为巨页
    pub const fn is_huge(&self) -> bool {
        (self.0 & _PAGE_HUGE) != 0
    }

    /// 检查是否全局
    pub const fn is_global(&self) -> bool {
        (self.0 & _PAGE_GLOBAL) != 0
    }

    /// 检查是否可写
    pub const fn is_writable(&self) -> bool {
        (self.0 & _PAGE_WRITE) != 0
    }

    /// 检查是否脏
    pub const fn is_dirty(&self) -> bool {
        (self.0 & _PAGE_DIRTY) != 0
    }

    /// 检查是否禁止执行
    pub const fn is_no_exec(&self) -> bool {
        (self.0 & _PAGE_NO_EXEC) != 0
    }

    /// 获取物理地址
    pub const fn phys_addr(&self) -> usize {
        ((self.0 & _PAGE_PFN_MASK) >> _PAGE_PFN_SHIFT << PAGE_SHIFT) as usize
    }

    /// 设置物理地址
    pub fn set_phys_addr(&mut self, paddr: usize) {
        let pfn = (paddr >> PAGE_SHIFT) as u64;
        self.0 = (self.0 & !_PAGE_PFN_MASK) | (pfn << _PAGE_PFN_SHIFT);
    }

    /// 获取特权级
    pub const fn plv(&self) -> u64 {
        (self.0 & _PAGE_PLV_MASK) >> _PAGE_PLV_SHIFT
    }

    /// 设置特权级
    pub fn set_plv(&mut self, plv: u64) {
        self.0 = (self.0 & !_PAGE_PLV_MASK) | ((plv & 0x3) << _PAGE_PLV_SHIFT);
    }

    /// 获取缓存属性
    pub const fn cache_attr(&self) -> u64 {
        (self.0 & _CACHE_MASK) >> _CACHE_SHIFT
    }

    /// 设置缓存属性
    pub fn set_cache_attr(&mut self, cache: u64) {
        self.0 = (self.0 & !_CACHE_MASK) | ((cache & 0x3) << _CACHE_SHIFT);
    }

    /// 设置有效位
    pub fn set_valid(&mut self, valid: bool) {
        if valid {
            self.0 |= _PAGE_VALID;
        } else {
            self.0 &= !_PAGE_VALID;
        }
    }

    /// 设置存在位
    pub fn set_present(&mut self, present: bool) {
        if present {
            self.0 |= _PAGE_PRESENT;
        } else {
            self.0 &= !_PAGE_PRESENT;
        }
    }

    /// 设置巨页位
    pub fn set_huge(&mut self, huge: bool) {
        if huge {
            self.0 |= _PAGE_HUGE;
        } else {
            self.0 &= !_PAGE_HUGE;
        }
    }

    /// 设置全局位
    pub fn set_global(&mut self, global: bool) {
        if global {
            self.0 |= _PAGE_GLOBAL;
        } else {
            self.0 &= !_PAGE_GLOBAL;
        }
    }

    /// 设置写位
    pub fn set_writable(&mut self, writable: bool) {
        if writable {
            self.0 |= _PAGE_WRITE;
        } else {
            self.0 &= !_PAGE_WRITE;
        }
    }

    /// 设置脏位
    pub fn set_dirty(&mut self, dirty: bool) {
        if dirty {
            self.0 |= _PAGE_DIRTY;
        } else {
            self.0 &= !_PAGE_DIRTY;
        }
    }

    /// 设置禁止执行位
    pub fn set_no_exec(&mut self, no_exec: bool) {
        if no_exec {
            self.0 |= _PAGE_NO_EXEC;
        } else {
            self.0 &= !_PAGE_NO_EXEC;
        }
    }

    /// 创建内核页表项
    pub fn kernel_page(paddr: usize, writable: bool, executable: bool) -> Self {
        let mut entry = Self::empty();
        entry.set_phys_addr(paddr);
        entry.set_valid(true);
        entry.set_present(true);
        entry.set_global(true);
        entry.set_plv(0); // PLV0 = 内核态
        entry.set_cache_attr(1); // CC = Coherent Cached
        entry.set_writable(writable);
        if writable {
            entry.set_dirty(true);
        }
        if !executable {
            entry.set_no_exec(true);
        }
        entry
    }

    /// 创建用户页表项
    pub fn user_page(paddr: usize, writable: bool, executable: bool) -> Self {
        let mut entry = Self::empty();
        entry.set_phys_addr(paddr);
        entry.set_valid(true);
        entry.set_present(true);
        entry.set_plv(3); // PLV3 = 用户态
        entry.set_cache_attr(1); // CC = Coherent Cached
        entry.set_writable(writable);
        if writable {
            entry.set_dirty(true);
        }
        if !executable {
            entry.set_no_exec(true);
        }
        entry
    }

    /// 创建设备映射页表项 (非缓存)
    pub fn device_page(paddr: usize) -> Self {
        let mut entry = Self::empty();
        entry.set_phys_addr(paddr);
        entry.set_valid(true);
        entry.set_present(true);
        entry.set_global(true);
        entry.set_plv(0);
        entry.set_cache_attr(0); // SUC = Strongly-ordered UnCached
        entry.set_writable(true);
        entry.set_dirty(true);
        entry.set_no_exec(true);
        entry
    }
}

impl PageTableEntry for Entry {
    fn valid(&self) -> bool {
        self.is_valid()
    }

    fn paddr(&self) -> PhysAddr {
        PhysAddr::new(self.phys_addr())
    }

    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.set_phys_addr(paddr.raw());
    }

    fn set_valid(&mut self, valid: bool) {
        Entry::set_valid(self, valid);
        if valid {
            self.set_present(true);
        }
    }

    fn is_huge(&self) -> bool {
        Entry::is_huge(self)
    }

    fn set_is_huge(&mut self, b: bool) {
        self.set_huge(b);
    }

    fn set_mem_config(&mut self, config: page_table_generic::MemConfig) {
        use page_table_generic::{AccessFlags, MemAttributes};

        // 设置访问权限
        let writable = config.access.contains(AccessFlags::WRITE);
        let executable = config.access.contains(AccessFlags::EXECUTE);

        self.set_writable(writable);
        if writable {
            self.set_dirty(true);
        }
        self.set_no_exec(!executable);

        // 设置缓存属性
        match config.attrs {
            MemAttributes::Normal => {
                // CC = Coherent Cached
                self.set_cache_attr(1);
            }
            MemAttributes::Device => {
                // SUC = Strongly-ordered UnCached
                self.set_cache_attr(0);
            }
            MemAttributes::Uncached => {
                // WUC = Weakly-ordered UnCached
                self.set_cache_attr(2);
            }
        }
    }

    fn mem_config(&self) -> page_table_generic::MemConfig {
        use page_table_generic::{AccessFlags, MemAttributes};

        let mut access = AccessFlags::READ;

        if self.is_writable() {
            access |= AccessFlags::WRITE;
        }

        if !self.is_no_exec() {
            access |= AccessFlags::EXECUTE;
        }

        // 根据 PLV 判断是否为用户态页面
        if self.plv() == 3 {
            access |= AccessFlags::LOWER;
        }

        // 根据缓存属性确定内存类型
        let attrs = match self.cache_attr() {
            0 => MemAttributes::Device,   // SUC
            1 => MemAttributes::Normal,   // CC
            2 => MemAttributes::Uncached, // WUC
            _ => MemAttributes::Normal,   // 默认为 Normal
        };

        page_table_generic::MemConfig { access, attrs }
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Entry")
            .field("raw", &format_args!("{:#018x}", self.0))
            .field("valid", &self.is_valid())
            .field("present", &self.is_present())
            .field("huge", &self.is_huge())
            .field("global", &self.is_global())
            .field("writable", &self.is_writable())
            .field("dirty", &self.is_dirty())
            .field("no_exec", &self.is_no_exec())
            .field("plv", &self.plv())
            .field("cache", &self.cache_attr())
            .field("paddr", &format_args!("{:#x}", self.phys_addr()))
            .finish()
    }
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
    let mut table = page_table_generic::PageTable::<Generic, _>::new(Ram).unwrap();
    let kernel_start_phys = virt_to_phys(Arch::kernel_code().as_ptr());
    let size = Arch::kernel_code().len().align_up(page_size());
    let kernel_start_virt = super::addrspace::VM_CODE_START;
    println!(
        "Relocating kernel from phys addr: {:#x} -> {:#x}",
        kernel_start_phys, kernel_start_virt
    );
    let mut pte = Entry::empty();
    pte.set_valid(true);
    pte.set_mem_config(MemConfig {
        access: page_table_generic::AccessFlags::READ
            | page_table_generic::AccessFlags::WRITE
            | page_table_generic::AccessFlags::EXECUTE,
        attrs: page_table_generic::MemAttributes::Normal,
    });

    table
        .map(&MapConfig {
            vaddr: kernel_start_virt.into(),
            paddr: kernel_start_phys.into(),
            size,
            pte,
            allow_huge: false,
            flush: false,
        })
        .unwrap();

    let table_addr = table.root_paddr();
    BOOT_TABLE.init(table);
    super::Arch::set_kernel_page_table(PageTableInfo {
        asid: 0,
        addr: table_addr.raw(),
    });

    let offset = kernel_start_virt - kernel_start_phys;
    let entry = virt_to_phys(crate::after_finally_relocate as _) + offset;
    println!(
        "Relocate kernel: table at {:#x}, offset {:#x}, entry {:#x}",
        table_addr.raw(),
        offset,
        entry
    );
    set_table_and_relocate_kernel(table_addr.raw(), offset, entry)
}

#[unsafe(naked)]
extern "C" fn set_table_and_relocate_kernel(table: usize, offset: usize, entry: usize) -> ! {
    naked_asm!(
        "
        
        ",
    )
}
