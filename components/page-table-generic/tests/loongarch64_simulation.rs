//! LoongArch64 页表映射场景模拟测试
//!
//! 模拟 LoongArch64 启动时的内核代码段映射场景，用于调试死循环问题

#![cfg(not(target_os = "none"))]

use page_table_generic::*;

mod mocks;
use mocks::*;

/// LoongArch64 Generic 配置的 Mock 实现
/// 模拟 crates/somehal/src/arch/loongarch64/paging.rs:962-992
///
/// LoongArch64 页表配置：
/// - 4级页表 (PGD->PUD->PMD->PTE)
/// - 4KB 页大小
/// - 每级 9 位索引 (512 项)
/// - 仅 PMD 级别 (Level 1) 支持 2MB 巨页
#[derive(Debug, Clone, Copy)]
pub struct LoongArch64Generic;

impl TableGeneric for LoongArch64Generic {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000; // 4KB

    // 各级索引位数数组 (从最高级到最低级: PGD -> PUD -> PMD -> PTE)
    // 对于 4KB 页: 每级 9 位
    const LEVEL_BITS: &[usize] = &[
        9, // Level 3 (PGD) - 9 bits
        9, // Level 2 (PUD) - 9 bits
        9, // Level 1 (PMD) - 9 bits
        9, // Level 0 (PTE) - 9 bits
    ];

    /// 大页最高支持级别 (PMD 级别，即 Level 1)
    /// 这与 LoongArch64 实际配置一致：仅支持 2MB 巨页
    const MAX_BLOCK_LEVEL: usize = 2; // PUD (level 2) 支持 2MB 巨页

    fn flush(vaddr: Option<VirtAddr>) {
        let _ = vaddr;
    }
}

/// 模拟 __kimage_va 宏
/// 参考 LoongArch64 的虚拟地址偏移计算
const fn __kimage_va_sim(phys: usize) -> usize {
    // 使用与真实 LoongArch64 相同的虚拟地址偏移
    // 参考 crates/somehal/src/arch/loongarch64/addrspace.rs
    const KERNEL_VIRT_OFFSET: usize = 0x9000_0000_0000;
    KERNEL_VIRT_OFFSET + phys
}

const MB: usize = 1024 * 1024;

/// 测试 LoongArch64 内核代码段映射
///
/// 模拟 crates/somehal/src/arch/loongarch64/paging.rs:1014-1124
/// 中的 `relocate_kernel_to_vm_code()` 函数的映射逻辑
#[test]
fn test_loongarch64_kernel_code_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, Fram4k>::new(Fram4k).unwrap();

    // 模拟 LoongArch64 启动时的内核代码段映射
    // 参数来自 relocate_kernel_to_vm_code() 中的实际调用
    let kernel_phys_start = 0x2000_0000usize; // 假设内核物理起始地址
    let kernel_virt_start = __kimage_va_sim(kernel_phys_start); // 高虚拟地址
    let kernel_size = 2 * MB; // 2MB 对齐

    println!("=== LoongArch64 内核代码映射测试 ===");
    println!("物理起始: {:#x}", kernel_phys_start);
    println!("虚拟起始: {:#x}", kernel_virt_start);
    println!("大小: {:#x} ({} MB)", kernel_size, kernel_size / MB);

    // 执行映射 - 这与实际启动代码中的调用一致
    let result = pg.map(&MapConfig {
        vaddr: kernel_virt_start.into(),
        paddr: kernel_phys_start.into(),
        size: kernel_size,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: true, // 允许 2MB 巨页
        flush: false,
    });

    assert!(result.is_ok(), "内核代码映射应该成功: {:?}", result.err());

    // 验证映射结果
    let mapped_count = pg.walk_valid().count();
    println!("映射的页面数量: {}", mapped_count);
    assert!(mapped_count > 0, "应该有有效的映射");

    // 验证地址可翻译
    let (translated_paddr, pte) = pg
        .translate(kernel_virt_start.into())
        .expect("起始地址应该可翻译");
    println!(
        "起始地址翻译: VA={:#x} -> PA={:#x}, Huge={}, Valid={}",
        kernel_virt_start,
        translated_paddr.raw(),
        pte.to_config(false).huge,
        pte.to_config(false).valid
    );

    assert_eq!(
        translated_paddr.raw(),
        kernel_phys_start,
        "物理地址应该匹配"
    );

    // 验证大页映射（如果使用了）
    if pte.to_config(false).huge {
        println!("✓ 使用了 2MB 巨页映射");
    }

    // 验证映射范围内的地址都可访问
    let test_addrs = [
        kernel_virt_start,
        kernel_virt_start + 0x1000,
        kernel_virt_start + MB,
        kernel_virt_start + 2 * MB - 0x1000,
    ];

    for &test_vaddr in &test_addrs {
        if test_vaddr < kernel_virt_start + kernel_size {
            assert!(
                pg.is_mapped(test_vaddr.into()),
                "地址 {:#x} 应该被映射",
                test_vaddr
            );
        }
    }

    println!("🎉 LoongArch64 内核代码映射测试通过！");
}

/// 测试边界条件
///
/// 测试各种页表边界地址的映射，确保 level_size 计算正确
#[test]
fn test_loongarch64_boundary_conditions() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, Fram4k>::new(Fram4k).unwrap();

    // 测试边界条件：跨页表项边界的映射
    // 这些地址位于各级页表的边界上
    // 注意：由于允许大页，需要确保每个测试映射不重叠
    // Test 3 使用不同的 PUD 条目，避免在大页范围内创建子页表
    let boundary_cases = [
        (
            0x9000_0000_0000usize,
            0usize,
            2 * MB,
            true,
            "PGD边界（Level 3）",
        ),
        (
            0x9000_2000_0000usize,
            1 * 1024 * MB,
            2 * MB,
            true,
            "PUD边界（Level 2）",
        ),
        (
            0x9000_2020_0000usize,
            1 * 1024 * MB + 2 * MB,
            2 * MB,
            true,
            "PMD边界（Level 1）",
        ),
        // Test 3 使用新的 PUD 条目，避免在大页下创建子页表
        (
            0x9000_3000_0000usize,
            1 * 1024 * MB + 4 * MB,
            4 * KB,
            false,
            "PTE边界（Level 0）",
        ),
    ];

    for (i, &(base_vaddr, base_paddr, size, allow_huge, desc)) in boundary_cases.iter().enumerate()
    {
        println!("=== 测试 {}: {} ===", i, desc);

        let result = pg.map(&MapConfig {
            vaddr: base_vaddr.into(),
            paddr: base_paddr.into(),
            size,
            pte: PteImpl::kernel_mode_config(),
            allow_huge,
            flush: false,
        });

        assert!(
            result.is_ok(),
            "边界地址 {:#x} ({}) 的映射应该成功: {:?}",
            base_vaddr,
            desc,
            result.err()
        );

        // 验证翻译
        if let Ok((pa, pte)) = pg.translate(base_vaddr.into()) {
            println!(
                "  VA={:#x} -> PA={:#x}, Huge={}, Valid={}",
                base_vaddr,
                pa.raw(),
                pte.to_config(false).huge,
                pte.to_config(false).valid
            );
        }
    }

    println!("🎉 边界条件测试通过！");
}

/// 测试多段连续映射
///
/// 模拟多个内核段的连续映射，测试地址递增逻辑
#[test]
fn test_loongarch64_multi_range_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, Fram4k>::new(Fram4k).unwrap();

    // 模拟多个连续的内核段映射
    // 类似于内核的代码段、数据段、BSS段等
    let base_vaddr = 0x9000_2000_0000usize;
    let base_paddr = 0x2000_0000usize;
    let segment_count = 3;

    println!("=== 多段连续映射测试 ===");
    println!("虚拟基址: {:#x}", base_vaddr);
    println!("物理基址: {:#x}", base_paddr);
    println!("段数量: {}", segment_count);

    // 映射三个连续的 2MB 段
    for i in 0..segment_count {
        let offset = i * 2 * MB;
        let vaddr = base_vaddr + offset;
        let paddr = base_paddr + offset;

        let result = pg.map(&MapConfig {
            vaddr: vaddr.into(),
            paddr: paddr.into(),
            size: 2 * MB,
            pte: PteImpl::kernel_mode_config(),
            allow_huge: true,
            flush: false,
        });

        assert!(
            result.is_ok(),
            "段 {} (VA={:#x}, PA={:#x}) 的映射应该成功: {:?}",
            i,
            vaddr,
            paddr,
            result.err()
        );

        println!("  段 {} 映射成功: VA={:#x} -> PA={:#x}", i, vaddr, paddr);
    }

    // 验证所有段都可翻译
    for i in 0..segment_count {
        let vaddr = base_vaddr + i * 2 * MB;
        assert!(
            pg.is_mapped(vaddr.into()),
            "段 {} 的起始地址 {:#x} 应该被映射",
            i,
            vaddr
        );

        // 验证物理地址正确
        if let Ok((pa, _)) = pg.translate(vaddr.into()) {
            let expected_paddr = base_paddr + i * 2 * MB;
            assert_eq!(pa.raw(), expected_paddr, "段 {} 的物理地址不匹配", i);
        }
    }

    println!("🎉 多段映射测试通过！");
}

/// 测试非对齐地址的映射
///
/// 测试当虚拟或物理地址未按 2MB 对齐时的行为
/// 应该降级到使用 4KB 普通页
#[test]
fn test_loongarch64_unaligned_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, Fram4k>::new(Fram4k).unwrap();

    // 使用非 2MB 对齐的地址（偏移 0x1000）
    let kernel_virt_start = __kimage_va_sim(0x2000_1000usize); // 未对齐
    let kernel_phys_start = 0x2000_1000usize;
    let kernel_size = 64 * 1024; // 64KB，小于 2MB

    println!("=== 非对齐地址映射测试 ===");
    println!("虚拟起始: {:#x} (未2MB对齐)", kernel_virt_start);
    println!("物理起始: {:#x} (未2MB对齐)", kernel_phys_start);
    println!("大小: {} KB", kernel_size / 1024);

    let result = pg.map(&MapConfig {
        vaddr: kernel_virt_start.into(),
        paddr: kernel_phys_start.into(),
        size: kernel_size,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: true, // 允许巨页，但由于不对齐应该不会使用
        flush: false,
    });

    assert!(result.is_ok(), "非对齐映射应该成功: {:?}", result.err());

    // 验证映射
    assert!(pg.is_mapped(kernel_virt_start.into()), "起始地址应该被映射");

    // 检查是否使用了大页（不应该，因为地址未对齐）
    if let Ok((_, pte)) = pg.translate(kernel_virt_start.into()) {
        println!(
            "使用的映射类型: {}",
            if pte.to_config(false).huge {
                "大页（2MB）"
            } else {
                "普通页（4KB）"
            }
        );
    }

    println!("🎉 非对齐地址映射测试通过！");
}

/// 测试大内存区域的映射
///
/// 测试映射大于 2MB 的区域，验证是否正确使用多个大页
#[test]
fn test_loongarch64_large_region_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, Fram4k>::new(Fram4k).unwrap();

    // 映射 8MB 的区域（应该使用 4 个 2MB 大页）
    let kernel_virt_start = __kimage_va_sim(0x3000_0000usize);
    let kernel_phys_start = 0x3000_0000usize;
    let kernel_size = 8 * MB;

    println!("=== 大区域映射测试 ===");
    println!("虚拟起始: {:#x}", kernel_virt_start);
    println!("物理起始: {:#x}", kernel_phys_start);
    println!("大小: {} MB", kernel_size / MB);

    let result = pg.map(&MapConfig {
        vaddr: kernel_virt_start.into(),
        paddr: kernel_phys_start.into(),
        size: kernel_size,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: true,
        flush: false,
    });

    assert!(result.is_ok(), "大区域映射应该成功: {:?}", result.err());

    // 验证多个地址点都可访问
    let test_offsets = [0, 2 * MB, 4 * MB, 6 * MB, 8 * MB - 0x1000];
    for offset in test_offsets {
        let test_vaddr = kernel_virt_start + offset;
        assert!(
            pg.is_mapped(test_vaddr.into()),
            "偏移 {} 处的地址 {:#x} 应该被映射",
            offset,
            test_vaddr
        );
    }

    // 统计映射的页数
    let mapped_count = pg.walk_valid().count();
    println!("映射的页面总数: {}", mapped_count);

    // 验证是否使用了大页
    let mut huge_page_count = 0;
    for entry in pg.walk_valid() {
        if entry.pte.to_config(false).huge {
            huge_page_count += 1;
        }
    }

    println!("使用的大页数量: {}", huge_page_count);
    assert_eq!(huge_page_count, 4, "8MB 区域应该使用 4 个 2MB 大页");

    println!("🎉 大区域映射测试通过！");
}
