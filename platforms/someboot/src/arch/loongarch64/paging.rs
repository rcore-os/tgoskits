//! LoongArch64 页表管理模块
//!
//! 参考 Linux kernel arch/loongarch/mm/tlb.c 和 arch/loongarch/include/asm/loongarch.h
//! 实现页表寄存器初始化和相关数据类型定义。

use core::arch::naked_asm;

use loongArch64::register::MemoryAccessType;
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

/// 4KB 页大小的 PS 值
#[cfg(page_size_4k)]
const PS: usize = 0x0c;
/// 16KB 页大小的 PS 值
#[cfg(page_size_16k)]
const PS: u64 = 0x0e;

/// 页内偏移位数
pub const PAGE_SHIFT: usize = PAGE_SIZE.trailing_zeros() as usize;

const PWCL_VALUE: usize = 12 | (9 << 5) | (21 << 10) | (9 << 15) | (30 << 20) | (9 << 25);
const PWCH_VALUE: usize = 39 | (9 << 6);

// ============================================================================
// 页表层级配置
// ============================================================================

/// 每个页表索引的位数 = PAGE_SHIFT - 3 (页表项为8字节)
pub const PTE_INDEX_BITS: usize = PAGE_SHIFT - 3;

/// 无效化所有 TLB 条目
#[inline(always)]
#[cfg_attr(axtest_coverage, coverage(off))]
pub fn local_flush_tlb_all() {
    unsafe {
        core::arch::asm!("dbar 0; tlbflush", options(nomem, nostack));
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

/// Installs the kernel page table and page-walker geometry.
///
/// This function is also used before a secondary CPU can address the final
/// kernel image. Keep it self-contained and free of calls into instrumented
/// register-access crates: coverage counters live at final kernel addresses.
#[cfg_attr(axtest_coverage, coverage(off))]
fn setup(root_paddr: usize) {
    assert_eq!(root_paddr & (PAGE_SIZE - 1), 0);
    unsafe {
        core::arch::asm!(
            "csrrd {stlbps}, {csr_stlbps}",
            "bstrins.d {stlbps}, {ps}, 5, 0",
            "csrwr {root}, {csr_pgdh}",
            "csrwr {root}, {csr_pgdl}",
            "dbar 0",
            "csrwr {stlbps}, {csr_stlbps}",
            "csrwr {pwcl}, {csr_pwcl}",
            "csrwr {pwch}, {csr_pwch}",
            stlbps = out(reg) _,
            ps = in(reg) PS,
            root = in(reg) root_paddr,
            pwcl = in(reg) PWCL_VALUE,
            pwch = in(reg) PWCH_VALUE,
            csr_pgdl = const 0x19,
            csr_pgdh = const 0x1a,
            csr_pwcl = const 0x1c,
            csr_pwch = const 0x1d,
            csr_stlbps = const 0x1e,
            options(nostack),
        );
    }
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

    // 刷新指令缓存，确保跳转后执行的是正确位置的指令
    unsafe {
        core::arch::asm!("ibar 0", options(nomem, nostack));
        core::arch::asm!("dbar 0", options(nomem, nostack));
    }

    relocate_kernel(v_entry, v_sp);
    unreachable!()
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

    let mut crmd_bits: usize;
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) crmd_bits, const 0x0);
    }
    crmd_bits &= !(1 << 3);
    crmd_bits |= 1 << 4;
    crmd_bits &= !(0b11 << 5);
    crmd_bits |= (MemoryAccessType::CoherentCached as usize) << 5;
    crmd_bits &= !(0b11 << 7);
    crmd_bits |= (MemoryAccessType::CoherentCached as usize) << 7;
    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) crmd_bits, const 0x0);
    }
    jump_to_secondary_entry(cpu_meta_paddr, meta.stack_top_virt, meta.entry_virt)
}

#[unsafe(naked)]
extern "C" fn jump_to_secondary_entry(_arg: usize, _sp: usize, _entry: usize) -> ! {
    naked_asm!(
        "
        ibar 0
        dbar 0
        move $sp, $a1
        jr $a2
        "
    )
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
