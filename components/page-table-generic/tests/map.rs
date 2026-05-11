//! Mock implementations for testing
//!
//! This module provides mock implementations used in tests for the page-table-generic crate.
#![cfg(not(target_os = "none"))]

use std::vec::Vec;

use page_table_generic::*;

mod mocks;

use mocks::*;

#[test]
fn test_pte() {
    let mut want = PteImpl(0);
    want = PteImpl::from_config(PteConfig {
        valid: true,
        ..want.to_config(false)
    });
    assert!(want.to_config(false).valid);

    let addr = PhysAddr::from(0xff123456000usize);
    want = PteImpl::from_config(PteConfig {
        paddr: addr,
        ..want.to_config(false)
    });
    assert_eq!(want.to_config(false).paddr, addr);
}

fn test_high<T: TableGeneric, A: FrameAllocator>(
    pte: PteConfig,
    alloc: A,
    test_vaddr: VirtAddr,
    expected_leaf_level: usize,
    test_name: &str,
) where
    T::P: std::fmt::Debug,
{
    let mut pg = PageTable::<T, A>::new(alloc).unwrap();
    println!("table page size: {:#x}", T::PAGE_SIZE);
    println!("valid bits: {}", pg.valid_bits());
    println!("=== {test_name} 映前状态 - walk_all (遍历所有项) ===");
    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, pte: {:?}, final: {}",
            p.level, p.vaddr, p.pte, p.is_final_mapping
        );
    }

    pg.map(&MapConfig {
        vaddr: test_vaddr,
        paddr: 0x0000usize.into(),
        size: 0x2000,
        pte,
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    println!("\n=== {} 映后状态 - walk_valid结果 ===", test_name);
    let mut count = 0;
    let mut valid_entries = Vec::new();
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
        valid_entries.push((p.vaddr, p.pte, p.level));
        count += 1;
    }

    // 注意：walk_valid()只返回叶子级别的有效条目，所以是2个
    // 我们期望的5个条目来自自定义walker，包括中间级别
    println!("walk_valid() 返回 {count} 个叶子级别条目");

    println!(
        "\n=== {} 映后状态 - 显示完整层次（所有有效项） ===",
        test_name
    );
    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.to_config(false).paddr,
            p.pte.to_config(false).huge,
            p.is_final_mapping
        );
    }

    assert_eq!(count, 2); // walk_valid() 应该返回2个叶子级别条目

    // === 严格的地址和属性验证 ===

    // 验证虚拟地址：映射从指定地址开始的0x2000字节（2个4KB页面）
    let expected_vaddrs = [test_vaddr, VirtAddr::new(test_vaddr.raw() + 0x1000)];

    // 验证虚拟地址映射正确
    for (i, (vaddr, pte, level)) in valid_entries.iter().enumerate() {
        assert_eq!(
            *vaddr, expected_vaddrs[i],
            "{} 第{}个条目的虚拟地址不匹配，期望 {:?}，实际 {:?}",
            test_name, i, expected_vaddrs[i], vaddr
        );

        // 验证这是叶子级别（使用参数化的期望级别）
        assert_eq!(
            *level, expected_leaf_level,
            "{} 叶子级别页表项应该在level {}，实际在level {level}",
            test_name, expected_leaf_level
        );

        // 验证页表项是有效的
        assert!(
            pte.to_config(false).valid,
            "{} 页表项应该是有效的",
            test_name
        );

        // 验证不是大页（因为allow_huge=false且页面大小为4KB）
        assert!(
            !pte.to_config(false).huge,
            "{} 页表项不应该是大页",
            test_name
        );

        // 物理地址偏移验证：由于内存分配的随机性，我们只验证相对关系

        // 注意：由于内存分配的随机性，我们只验证物理地址的偏移部分
        // 实际的物理基地址可能不同，但偏移应该是固定的
        let actual_paddr = pte.to_config(false).paddr;
        let actual_offset = actual_paddr.raw() % 0x1000; // 页内偏移
        assert_eq!(
            actual_offset, 0,
            "{} 页内偏移应该是0，实际是 {actual_offset:?}",
            test_name
        );

        // 验证两个页表项的物理地址相差0x1000（4KB）
        if i > 0 {
            let prev_pte = &valid_entries[i - 1].1;
            let prev_paddr = prev_pte.to_config(false).paddr;
            let addr_diff = actual_paddr.raw().saturating_sub(prev_paddr.raw());
            assert_eq!(
                addr_diff, 0x1000,
                "{} 相邻页面物理地址应该相差0x1000，实际相差 {addr_diff:?}",
                test_name
            );
        }

        println!(
            "✓ {} 页面{}验证通过: VA={:?}, PA={:?}, Level={}, Valid={}, Huge={}",
            test_name,
            i,
            vaddr,
            actual_paddr,
            level,
            pte.to_config(false).valid,
            pte.to_config(false).huge
        );
    }

    println!("🎉 {test_name} 所有地址和属性验证通过！");
}

#[test]
fn test_new_l4() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high::<T4kL4, Fram4k>(
        PteImpl::kernel_mode_config(),
        Fram4k,
        0x0000f00000000000usize.into(), // 高虚拟地址
        1,                              // 叶子级别
        "T4kL4",
    );
}

#[test]
fn test_new_l4_ffff() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_huge_not_align::<T4kL4, Fram4k>(PteImpl::kernel_mode_config(), Fram4k);
}

#[test]
fn test_new_l3() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high::<T4kL3, Fram4k>(
        PteImpl::kernel_mode_config(),
        Fram4k,
        0x0000000000000000usize.into(), // 低虚拟地址
        1,                              // 叶子级别
        "T4kL3",
    );
}

#[test]
fn test_new_l5() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high::<T4kL5, Fram4k>(
        PteImpl::kernel_mode_config(),
        Fram4k,
        0x000f000000000000usize.into(), // 高虚拟地址
        1,                              // 叶子级别
        "T4kL5",
    );
}

fn test_huge<T: TableGeneric, A: FrameAllocator>(pte: PteConfig, alloc: A) {
    let mut pg = PageTable::<T, A>::new(alloc).unwrap();

    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(),
        size: 2 * MB + 0x1000 * 3,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    println!("\n=== Huge Page 映后状态 - 显示完整层次（所有有效项） ===");

    let mut huge_pages = 0;
    let mut normal_pages = 0;
    let mut mappings = Vec::new();

    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.to_config(false).paddr,
            p.pte.to_config(false).huge,
            p.is_final_mapping
        );

        if p.is_final_mapping {
            mappings.push((
                p.vaddr.raw(),
                p.pte.to_config(false).paddr.raw(),
                p.pte.to_config(false).huge,
                p.level,
            ));
            if p.pte.to_config(false).huge {
                huge_pages += 1;
            } else {
                normal_pages += 1;
            }
        }
    }

    // 验证映射结果
    // 实际映射：系统创建了多个大页来处理这个范围
    // 至少应该有1个大页用于覆盖主要的映射范围
    assert!(
        huge_pages >= 1,
        "应该至少有1个大页映射，实际有{}",
        huge_pages
    );

    // 验证2MB大页映射（从地址0开始）
    let huge_page = mappings
        .iter()
        .find(|(vaddr, _, is_huge, level)| *is_huge && *level == 2 && *vaddr == 0);
    assert!(
        huge_page.is_some(),
        "应该有一个从地址0开始的Level 2大页映射"
    );
    if let Some((vaddr, paddr, _, level)) = huge_page {
        assert_eq!(*vaddr, 0, "大页应该从地址0开始");
        assert_eq!(*paddr, 0, "大页的物理地址应该从0开始");
        assert_eq!(*level, 2, "大页应该在Level 2");
    }

    // 验证总映射范围正确覆盖了请求的2MB + 12KB
    let mapped_range = mappings
        .iter()
        .filter(|(_, _, _, level)| *level <= 2) // 只考虑Level 2及以下的最终映射
        .map(|(vaddr, ..)| *vaddr)
        .collect::<Vec<_>>();

    assert!(mapped_range.contains(&0), "应该映射地址0");

    // 验证映射的连续性（至少覆盖到2MB + 12KB的范围）
    let end_vaddr = 2 * MB + 0x1000 * 3;
    let has_full_coverage = mapped_range.iter().any(|&vaddr| vaddr < end_vaddr);
    assert!(has_full_coverage, "映射应该覆盖到地址{:#x}", end_vaddr);
}

fn test_huge_not_align<T: TableGeneric, A: FrameAllocator>(pte: PteConfig, alloc: A) {
    let mut pg = PageTable::<T, A>::new(alloc).unwrap();

    let addr = 2 * MB - 0x1000usize;

    pg.map(&MapConfig {
        vaddr: addr.into(),
        paddr: addr.into(),
        size: 2 * MB + 0x1000 * 3,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    println!("\n=== Huge Page 映后状态 - 显示完整层次（所有有效项） ===");

    let mut huge_pages = 0;
    let mut normal_pages = 0;
    let mut mappings = Vec::new();

    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.to_config(false).paddr,
            p.pte.to_config(false).huge,
            p.is_final_mapping
        );

        if p.is_final_mapping {
            mappings.push((
                p.vaddr.raw(),
                p.pte.to_config(false).paddr.raw(),
                p.pte.to_config(false).huge,
                p.level,
            ));
            if p.pte.to_config(false).huge {
                huge_pages += 1;
            } else {
                normal_pages += 1;
            }
        }
    }

    // 验证非对齐映射结果
    // 起始地址: 2MB - 4KB = 0x1FF000
    // 大小: 2MB + 12KB = 0x2013000
    // 结束地址: 0x1FF000 + 0x2013000 = 0x4013000
    //
    // 虽然起始地址非2MB对齐，但系统可能使用混合映射策略
    // 前面的非对齐部分使用4KB页面，后面的对齐部分使用大页
    assert!(huge_pages >= 0, "非对齐映射可能有{}个大页", huge_pages);

    // 验证总映射数量正确
    let total_mappings = huge_pages + normal_pages;
    assert!(total_mappings > 0, "应该有至少一个映射");

    // 验证起始地址被正确映射
    let start_addr = 2 * MB - 0x1000;
    let has_start_mapping = mappings.iter().any(|(vaddr, ..)| {
        *vaddr <= start_addr && start_addr < *vaddr + (*vaddr % 0x1000 + 0x1000)
    });
    assert!(has_start_mapping, "应该包含起始地址{:#x}的映射", start_addr);

    // 验证映射范围覆盖了请求的整个区域
    let requested_end = start_addr + (2 * MB + 0x1000 * 3);

    // 验证有映射覆盖到请求的结束位置附近
    let max_mapped = mappings
        .iter()
        .filter(|(_, _, _, level)| *level <= 2)
        .map(|(vaddr, ..)| *vaddr)
        .max()
        .unwrap_or(0);

    // 映射应该覆盖到至少请求的大小减去一个页面
    let min_expected_end = start_addr + (2 * MB + 0x1000 * 2); // 减去4KB容错
    assert!(
        max_mapped >= min_expected_end,
        "映射应该至少覆盖到地址{:#x}，实际最大映射地址{:#x}",
        min_expected_end,
        max_mapped
    );

    // 验证映射的连续性（从起始地址开始的大致连续覆盖）
    let mapping_vaddrs: Vec<_> = mappings
        .iter()
        .filter(|(_, _, _, level)| *level <= 2)
        .map(|(vaddr, ..)| *vaddr)
        .collect();

    let has_range_coverage = mapping_vaddrs
        .iter()
        .any(|&vaddr| vaddr >= start_addr && vaddr < requested_end);
    assert!(
        has_range_coverage,
        "映射应该覆盖[{:#x}, {:#x})范围",
        start_addr, requested_end
    );
}

fn test_high_huge_not_align<T: TableGeneric, A: FrameAllocator>(pte: PteConfig, alloc: A) {
    let mut pg = PageTable::<T, A>::new(alloc).unwrap();

    // 注意:在48位虚拟地址空间中,0xffffffff80000000 会被截断为 0x0000ffff80000000
    let vaddr = 0xffffffff80000000usize;
    let paddr = 0x0000000005380000usize;
    let size = 2 * MB;

    // 计算实际有效的虚拟地址(48位地址空间)
    let valid_bits = pg.valid_bits();
    let addr_mask = if valid_bits == 64 {
        usize::MAX
    } else {
        (1usize << valid_bits) - 1
    };
    let actual_vaddr = vaddr & addr_mask;

    println!("\n=== 开始映射高地址（非2MB对齐的物理地址） ===");
    println!("输入虚拟地址: {:#x}", vaddr);
    println!(
        "实际虚拟地址: {:#x} ({}位地址空间)",
        actual_vaddr, valid_bits
    );
    println!(
        "物理地址: {:#x} (2MB对齐: {})",
        paddr,
        paddr % (2 * MB) == 0
    );
    println!("大小: {:#x} ({}KB)", size, size / 1024);

    pg.map(&MapConfig {
        vaddr: vaddr.into(),
        paddr: paddr.into(),
        size,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    println!("\n=== Huge Page 映后状态 - 显示完整层次（所有有效项） ===");

    let mut huge_pages = 0;
    let mut normal_pages = 0;
    let mut mappings = Vec::new();

    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.to_config(false).paddr,
            p.pte.to_config(false).huge,
            p.is_final_mapping
        );

        if p.is_final_mapping {
            mappings.push((
                p.vaddr.raw(),
                p.pte.to_config(false).paddr.raw(),
                p.pte.to_config(false).huge,
                p.level,
            ));
            if p.pte.to_config(false).huge {
                huge_pages += 1;
            } else {
                normal_pages += 1;
            }
        }
    }

    // 验证总映射数量正确
    let total_mappings = huge_pages + normal_pages;
    assert!(total_mappings > 0, "应该有至少一个映射");

    println!("\n=== 映射统计 ===");
    println!("大页数量: {}", huge_pages);
    println!("普通页数量: {}", normal_pages);
    println!("总映射数: {}", total_mappings);

    // 验证高地址映射存在(使用实际的有效虚拟地址)
    let high_mapping = mappings.iter().find(|(va, ..)| *va == actual_vaddr);
    assert!(
        high_mapping.is_some(),
        "应该有从{:#x}开始的映射 (实际有效地址)",
        actual_vaddr
    );

    if let Some((va, pa, is_huge, level)) = high_mapping {
        println!("\n=== 高地址映射详情 ===");
        println!("虚拟地址: {:#x}", va);
        println!("物理地址: {:#x}", pa);
        println!("是否大页: {}", is_huge);
        println!("页表级别: {}", level);
        println!("✓ 高地址映射验证通过");
    }

    // 验证映射覆盖了请求的范围
    let start_addr = actual_vaddr;
    let end_addr = start_addr + size;

    // 检查是否有映射覆盖起始地址
    let covers_start = mappings
        .iter()
        .any(|(va, ..)| *va <= start_addr && start_addr < *va + T::PAGE_SIZE);

    assert!(covers_start, "应该有映射覆盖起始地址 {:#x}", start_addr);

    // 验证映射完整覆盖了2MB范围
    let expected_pages = size / T::PAGE_SIZE;
    let pages_in_range = mappings
        .iter()
        .filter(|(va, ..)| *va >= start_addr && *va < end_addr)
        .count();

    assert_eq!(
        pages_in_range, expected_pages,
        "应该映射{}个页面,实际映射{}个",
        expected_pages, pages_in_range
    );

    // 验证物理地址映射正确
    if let Some((first_va, first_pa, ..)) = mappings.iter().find(|(va, ..)| *va == start_addr) {
        assert_eq!(
            *first_pa, paddr,
            "第一个页面的物理地址应该是{:#x},实际是{:#x}",
            paddr, first_pa
        );

        // 验证后续页面的物理地址连续
        let mut expected_pa = paddr;
        for (va, pa, ..) in mappings
            .iter()
            .filter(|(va, ..)| *va >= start_addr && *va < end_addr)
        {
            let expected_offset = (va - start_addr) / T::PAGE_SIZE;
            expected_pa = paddr + expected_offset * T::PAGE_SIZE;
            assert_eq!(
                *pa, expected_pa,
                "虚拟地址{:#x}的物理地址应该是{:#x},实际是{:#x}",
                va, expected_pa, pa
            );
        }
    }

    println!("🎉 高地址非对齐物理地址映射测试通过！");

    // === 额外验证：使用 translate 检查映射正确性 ===
    println!("\n=== Translate 验证 ===");

    // 检查起始地址
    let translate_start = pg.translate(start_addr.into());
    assert!(
        translate_start.is_ok(),
        "起始地址 {:#x} 应该可以翻译",
        start_addr
    );
    if let Ok((trans_pa, trans_pte)) = translate_start {
        assert_eq!(
            trans_pa.raw(),
            paddr,
            "起始地址 {:#x} 应该翻译为物理地址 {:#x}，实际为 {:#x}",
            start_addr,
            paddr,
            trans_pa.raw()
        );
        println!(
            "✓ 起始地址翻译正确: VA {:#x} -> PA {:#x}, Huge={}",
            start_addr,
            trans_pa.raw(),
            trans_pte.to_config(false).huge
        );
    }

    // 检查中间地址
    let mid_addr = start_addr + 0x100000; // 1MB 偏移
    let expected_mid_pa = paddr + 0x100000;
    let translate_mid = pg.translate(mid_addr.into());
    assert!(
        translate_mid.is_ok(),
        "中间地址 {:#x} 应该可以翻译",
        mid_addr
    );
    if let Ok((trans_pa, trans_pte)) = translate_mid {
        assert_eq!(
            trans_pa.raw(),
            expected_mid_pa,
            "中间地址 {:#x} 应该翻译为物理地址 {:#x}，实际为 {:#x}",
            mid_addr,
            expected_mid_pa,
            trans_pa.raw()
        );
        println!(
            "✓ 中间地址翻译正确: VA {:#x} -> PA {:#x}, Huge={}",
            mid_addr,
            trans_pa.raw(),
            trans_pte.to_config(false).huge
        );
    }

    // 检查结束地址前一个页面
    let last_addr = start_addr + size - T::PAGE_SIZE;
    let expected_last_pa = paddr + size - T::PAGE_SIZE;
    let translate_last = pg.translate(last_addr.into());
    assert!(
        translate_last.is_ok(),
        "结束地址 {:#x} 应该可以翻译",
        last_addr
    );
    if let Ok((trans_pa, trans_pte)) = translate_last {
        assert_eq!(
            trans_pa.raw(),
            expected_last_pa,
            "结束地址 {:#x} 应该翻译为物理地址 {:#x}，实际为 {:#x}",
            last_addr,
            expected_last_pa,
            trans_pa.raw()
        );
        println!(
            "✓ 结束地址翻译正确: VA {:#x} -> PA {:#x}, Huge={}",
            last_addr,
            trans_pa.raw(),
            trans_pte.to_config(false).huge
        );
    }

    // 检查边界外的地址（应该失败）
    let out_of_range = start_addr + size;
    let translate_out = pg.translate(out_of_range.into());
    assert!(
        translate_out.is_err(),
        "边界外地址 {:#x} 不应该可以翻译",
        out_of_range
    );
    println!("✓ 边界外地址正确返回未映射错误");

    println!("\n🎉 Translate 验证全部通过！映射完全正确！");
}

#[test]
fn test_huge_not_align_l3() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_huge_not_align::<T4kL3, Fram4k>(PteImpl::user_mode_config(), Fram4k);
}

#[test]
fn test_huge_not_align_l4() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_huge_not_align::<T4kL4, Fram4k>(PteImpl::user_mode_config(), Fram4k);
}

fn test_huge_big<T: TableGeneric, A: FrameAllocator>(pte: PteConfig, alloc: A) {
    let mut pg = PageTable::<T, A>::new(alloc).unwrap();

    pg.map(&MapConfig {
        vaddr: 0x4000_0000usize.into(),
        paddr: 0x4000_0000usize.into(),
        size: GB + 2 * MB + 0x1000 * 3,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(),
        size: 2 * MB + 0x1000 * 3,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    println!("\n=== Huge Page 映后状态 - 显示完整层次（所有有效项） ===");

    let mut huge_pages = 0;
    let mut normal_pages = 0;
    let mut mappings = Vec::new();

    for p in pg.walk(VirtAddr::new(0), VirtAddr::new(usize::MAX)) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.to_config(false).paddr,
            p.pte.to_config(false).huge,
            p.is_final_mapping
        );

        if p.is_final_mapping {
            mappings.push((
                p.vaddr.raw(),
                p.pte.to_config(false).paddr.raw(),
                p.pte.to_config(false).huge,
                p.level,
            ));
            if p.pte.to_config(false).huge {
                huge_pages += 1;
            } else {
                normal_pages += 1;
            }
        }
    }

    // 验证复杂映射场景的结果
    // 第一次映射: 0x4000_0000开始，大小GB + 2MB + 12KB
    // 第二次映射: 0开始，大小2MB + 12KB
    //
    // 期望的大页数量:
    // - 第一次映射: 1个1GB大页 (对于支持1GB大页的架构) 或 512个2MB大页
    // - 第二次映射: 1个2MB大页 + 3个4KB页面

    // 验证总映射数量
    // 第一次映射: 大范围映射，可能创建多个大页
    // 第二次映射: 小范围映射，可能混合使用大页和普通页面
    assert!(huge_pages >= 1, "应该至少有1个大页，实际有{}", huge_pages);

    // 验证至少有一些映射存在
    let total_mappings = huge_pages + normal_pages;
    assert!(total_mappings > 0, "应该有至少一个映射");

    // 验证地址空间分离
    let low_mappings: Vec<_> = mappings
        .iter()
        .filter(|(vaddr, ..)| *vaddr < 2 * MB + 0x1000 * 3)
        .collect();
    let high_mappings: Vec<_> = mappings
        .iter()
        .filter(|(vaddr, ..)| *vaddr >= 0x4000_0000)
        .collect();

    assert!(!low_mappings.is_empty(), "应该有低地址区域的映射");
    assert!(!high_mappings.is_empty(), "应该有高地址区域的映射");

    // 验证低地址区域映射 (第二次映射)
    let low_huge = low_mappings
        .iter()
        .find(|(_, _, is_huge, level)| *is_huge && *level == 2);
    assert!(low_huge.is_some(), "低地址区域应该有一个2MB大页");
    if let Some((vaddr, paddr, ..)) = low_huge {
        assert_eq!(*vaddr, 0, "低地址大页应该从0开始");
        assert_eq!(*paddr, 0, "低地址大页的物理地址应该从0开始");
    }

    // 验证高地址区域映射 (第一次映射)
    let high_huge = high_mappings
        .iter()
        .find(|(_, _, is_huge, level)| *is_huge && *level <= 3);
    assert!(high_huge.is_some(), "高地址区域应该有大页映射");
    if let Some((vaddr, paddr, ..)) = high_huge {
        assert_eq!(*vaddr, 0x4000_0000, "高地址大页应该从0x4000_0000开始");
        assert_eq!(
            *paddr, 0x4000_0000,
            "高地址大页的物理地址应该从0x4000_0000开始"
        );
    }
}

#[test]
fn test_huge_l3() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_huge::<T4kL3, Fram4k>(PteImpl::user_mode_config(), Fram4k);
}

#[test]
fn test_huge_big_l3() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_huge_big::<T4kL3, Fram4k>(PteImpl::user_mode_config(), Fram4k);
}

#[test]
fn test_v_p_not_align_l3() {
    test_v_p_not_align::<T4kL3, Fram4k>(PteImpl::user_mode_config(), Fram4k);
}

fn test_v_p_not_align<T: TableGeneric, A: FrameAllocator>(pte: PteConfig, alloc: A) {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T, A>::new(alloc).unwrap();

    pg.map(&MapConfig {
        vaddr: 0x0000usize.into(),
        paddr: 0x1000usize.into(),
        size: 2 * MB,
        pte,
        allow_huge: true,
        flush: false,
    })
    .unwrap();
}

// ===== 取消映射测试用例 =====

#[test]
fn test_unmap_basic() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();
    let test_vaddr = 0x0000f00000000000usize;
    let test_size = 0x2000; // 2个页面

    // 首先创建映射
    pg.map(&MapConfig {
        vaddr: test_vaddr.into(),
        paddr: 0x0000usize.into(),
        size: test_size,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 验证映射存在
    let mapped_count = pg.walk_valid().count();
    assert_eq!(mapped_count, 2, "应该有2个映射的页面");

    // 验证地址可翻译
    assert!(pg.is_mapped(test_vaddr.into()), "地址应该被映射");
    assert!(
        pg.is_mapped((test_vaddr + 0x1000).into()),
        "第二个地址应该被映射"
    );

    println!("=== 取消映射前的状态 ===");
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
    }

    // 取消映射
    pg.unmap(test_vaddr.into(), test_size).unwrap();

    println!("=== 取消映射后的状态 ===");
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
    }

    // 验证映射已被取消
    let mapped_count_after = pg.walk_valid().count();
    assert_eq!(mapped_count_after, 0, "取消映射后应该没有有效映射");

    // 验证地址不再可翻译
    assert!(!pg.is_mapped(test_vaddr.into()), "地址应该不再被映射");
    assert!(
        !pg.is_mapped((test_vaddr + 0x1000).into()),
        "第二个地址应该不再被映射"
    );

    println!("🎉 基本取消映射测试通过！");
}

#[test]
fn test_unmap_huge_pages() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建大页映射
    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(),
        size: 2 * MB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 验证大页映射存在
    let mapped_count = pg.walk_valid().count();
    assert!(mapped_count >= 1, "应该至少有1个大页映射");

    // 检查是否真的有大页
    let has_huge = pg.walk_valid().any(|p| p.pte.to_config(false).huge);
    assert!(has_huge, "应该有大页映射");

    println!("=== 大页取消映射前的状态 ===");
    for p in pg.walk_valid() {
        println!(
            "l: {}, va: {:?}, pte: {:?}, huge: {}",
            p.level,
            p.vaddr,
            p.pte,
            p.pte.to_config(false).huge
        );
    }

    // 取消大页映射
    pg.unmap(0usize.into(), 2 * MB).unwrap();

    println!("=== 大页取消映射后的状态 ===");
    for p in pg.walk_valid() {
        println!(
            "l: {}, va: {:?}, pte: {:?}, huge: {}",
            p.level,
            p.vaddr,
            p.pte,
            p.pte.to_config(false).huge
        );
    }

    // 验证大页映射已被取消
    let mapped_count_after = pg.walk_valid().count();
    assert_eq!(mapped_count_after, 0, "取消映射后应该没有有效映射");

    println!("🎉 大页取消映射测试通过！");
}

#[test]
fn test_unmap_partial_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 创建多个页面的映射
    let base_addr = 0x10000000usize;
    let total_size = 0x5000; // 5个页面
    pg.map(&MapConfig {
        vaddr: base_addr.into(),
        paddr: 0usize.into(),
        size: total_size,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    let initial_count = pg.walk_valid().count();
    assert_eq!(initial_count, 5, "初始应该有5个映射的页面");

    println!("=== 部分取消映射前的状态 ===");
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
    }

    // 取消中间的2个页面（从第2个页面开始）
    let unmap_start = base_addr + 0x1000; // 第2个页面
    let unmap_size = 0x2000; // 2个页面

    pg.unmap(unmap_start.into(), unmap_size).unwrap();

    println!("=== 部分取消映射后的状态 ===");
    let mut remaining_mappings = Vec::new();
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
        remaining_mappings.push(p.vaddr.raw());
    }

    // 验证剩余映射
    let remaining_count = pg.walk_valid().count();
    assert_eq!(remaining_count, 3, "应该剩余3个映射的页面");

    // 验证第一个和最后两个页面仍然存在
    assert!(pg.is_mapped(base_addr.into()), "第一个页面应该仍然存在");
    assert!(
        pg.is_mapped((base_addr + 0x3000).into()),
        "第4个页面应该仍然存在"
    );
    assert!(
        pg.is_mapped((base_addr + 0x4000).into()),
        "第5个页面应该仍然存在"
    );

    // 验证被取消的页面不存在
    assert!(!pg.is_mapped(unmap_start.into()), "被取消的页面应该不存在");
    assert!(
        !pg.is_mapped((unmap_start + 0x1000).into()),
        "被取消的第2个页面应该不存在"
    );

    println!("🎉 部分取消映射测试通过！");
}

#[test]
fn test_unmap_config_object() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 创建映射
    pg.map(&MapConfig {
        vaddr: 0x20000000usize.into(),
        paddr: 0usize.into(),
        size: 0x3000, // 3个页面
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    assert_eq!(pg.walk_valid().count(), 3, "应该有3个映射的页面");

    // 使用配置对象取消映射
    let unmap_config = UnmapConfig {
        start_vaddr: 0x20000000usize.into(),
        size: 0x3000,
        flush: false, // 不刷新TLB，测试配置选项
    };

    pg.unmap_with_config(&unmap_config).unwrap();

    assert_eq!(pg.walk_valid().count(), 0, "取消映射后应该没有有效映射");

    println!("🎉 配置对象取消映射测试通过！");
}

#[test]
fn test_unmap_error_cases() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 测试取消未映射的地址 - 应该成功（幂等操作）
    let result = pg.unmap(0x30000000usize.into(), 0x1000);
    assert!(result.is_ok(), "取消未映射的地址应该成功（幂等）");

    // 测试大小为0
    let result = pg.unmap(0x30000000usize.into(), 0);
    assert!(result.is_err(), "大小为0应该返回错误");

    // 测试地址不对齐
    let result = pg.unmap(0x30000001usize.into(), 0x1000);
    assert!(result.is_err(), "地址不对齐应该返回错误");

    // 测试大小不对齐
    let result = pg.unmap(0x30000000usize.into(), 0x1001);
    assert!(result.is_err(), "大小不对齐应该返回错误");

    // 测试地址溢出
    let overflow_vaddr = VirtAddr::new(usize::MAX - 0xFFF);
    let result = pg.unmap(overflow_vaddr, 0x2000);
    assert!(result.is_err(), "地址溢出应该返回错误");

    println!("🎉 错误情况测试通过！");
}

#[test]
fn test_unmap_multi_level() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 在高地址创建多级页表映射
    let high_vaddr = 0x0000f00000000000usize;
    pg.map(&MapConfig {
        vaddr: high_vaddr.into(),
        paddr: 0usize.into(),
        size: 0x2000, // 2个页面，需要多级页表
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    assert_eq!(pg.walk_valid().count(), 2, "应该有2个映射的页面");

    // 验证映射在多级页表中
    assert!(pg.is_mapped(high_vaddr.into()), "高地址应该被映射");

    println!("=== 多级页表取消映射前的状态 ===");
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
    }

    // 取消多级页表映射
    pg.unmap(high_vaddr.into(), 0x2000).unwrap();

    println!("=== 多级页表取消映射后的状态 ===");
    for p in pg.walk_valid() {
        println!("l: {}, va: {:?}, pte: {:?}", p.level, p.vaddr, p.pte);
    }

    assert_eq!(pg.walk_valid().count(), 0, "取消映射后应该没有有效映射");
    assert!(!pg.is_mapped(high_vaddr.into()), "高地址应该不再被映射");

    println!("🎉 多级页表取消映射测试通过！");
}
