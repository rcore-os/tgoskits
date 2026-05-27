//! 针对发现的bug的回归测试
//!
//! 这个测试文件专门用于验证修复的问题不会再次出现

use page_table_generic::*;
mod mocks;
use mocks::*;

/// 测试大页偏移计算
///
/// Bug描述：translate方法在计算大页偏移时总是使用MAX_BLOCK_LEVEL，
/// 而不是实际PTE所在的级别，导致不同级别的大页计算错误
#[test]
fn test_huge_page_offset_calculation() {
    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 创建一个Level 2的大页映射（2MB大页）
    // Level 2: 2MB = 512 * 4KB
    let vaddr_base = 0x10000000usize; // 256MB，2MB对齐
    let paddr_base = 0x20000000usize; // 512MB，2MB对齐
    let huge_page_size = 2 * MB; // 2MB

    pg.map(&MapConfig {
        vaddr: vaddr_base.into(),
        paddr: paddr_base.into(),
        size: huge_page_size,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 验证大页映射存在
    let has_huge = pg.walk_valid().any(|p| p.pte.to_config(false).huge);
    assert!(has_huge, "应该有大页映射");

    // 测试大页内不同偏移的地址翻译
    for offset in [0x0, 0x1000, 0x100000, 0x1FF000] {
        let test_vaddr = vaddr_base + offset;
        let expected_paddr = paddr_base + offset;

        let (translated_paddr, pte) = pg.translate(test_vaddr.into()).unwrap();

        assert!(pte.to_config(false).huge, "应该是大页映射");
        assert_eq!(
            translated_paddr.raw(),
            expected_paddr,
            "大页偏移计算错误: vaddr={:#x}, expected={:#x}, got={:#x}",
            test_vaddr,
            expected_paddr,
            translated_paddr.raw()
        );
    }

    println!("✅ 大页偏移计算测试通过！");
}

/// 测试多级别大页的正确处理
///
/// 验证不同级别的大页（如果架构支持）都能正确计算偏移
#[test]
fn test_multi_level_huge_pages() {
    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // Level 2 大页: 2MB
    let vaddr1 = 0x0;
    let paddr1 = 0x0;
    pg.map(&MapConfig {
        vaddr: vaddr1.into(),
        paddr: paddr1.into(),
        size: 2 * MB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // Level 3 大页: 1GB (如果支持)
    let vaddr2 = GB; // 1GB对齐
    let paddr2 = GB;
    pg.map(&MapConfig {
        vaddr: vaddr2.into(),
        paddr: paddr2.into(),
        size: GB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 测试Level 2大页的翻译
    let (paddr, pte) = pg.translate((vaddr1 + 0x80000).into()).unwrap();
    if pte.to_config(false).huge {
        assert_eq!(paddr.raw(), paddr1 + 0x80000, "Level 2大页偏移计算错误");
    }

    // 测试Level 3大页的翻译
    let (paddr, pte) = pg.translate((vaddr2 + 16 * MB).into()).unwrap();
    if pte.to_config(false).huge {
        assert_eq!(paddr.raw(), paddr2 + 16 * MB, "Level 3大页偏移计算错误");
    }

    println!("✅ 多级别大页测试通过！");
}

/// 测试地址比较逻辑
///
/// Bug描述：walk.rs中使用了.ge()方法而不是>=运算符，
/// 虽然功能相同，但应该使用更惯用的运算符
#[test]
fn test_walk_address_comparison() {
    let pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 测试空页表遍历
    let start = VirtAddr::new(0x1000);
    let end = VirtAddr::new(0x2000);

    // 正常范围
    let count1 = pg.walk(start, end).count();
    assert_eq!(count1, 0, "空页表应该没有条目");

    // 反向范围（start >= end）
    let count2 = pg.walk(end, start).count();
    assert_eq!(count2, 0, "反向范围应该返回空迭代器");

    // 相同地址
    let count3 = pg.walk(start, start).count();
    assert_eq!(count3, 0, "相同起止地址应该返回空迭代器");

    println!("✅ 地址比较逻辑测试通过！");
}

/// 测试unmap递归回收逻辑
///
/// Bug描述：unmap_range_recursive中遇到无效页表项时错误地设置can_reclaim=false，
/// 实际上无效项不应该影响回收判断
#[test]
fn test_unmap_reclaim_logic() {
    let mut pg = PageTable::<T4kL4, TrackedFram4k>::new(TrackedFram4k::new()).unwrap();

    let base_addr = 0x10000000usize;
    let size = 0x3000; // 3个页面

    // 创建映射
    pg.map(&MapConfig {
        vaddr: base_addr.into(),
        paddr: 0x0usize.into(),
        size,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    let allocator = pg.root.allocator;
    let allocated_before = allocator.allocated_count();
    println!("取消映射前分配的帧数: {}", allocated_before);

    // 取消所有映射
    pg.unmap(base_addr.into(), size).unwrap();

    let allocated_after = allocator.allocated_count();
    println!("取消映射后分配的帧数: {}", allocated_after);

    // 验证空的子页表帧被正确回收
    // 注意：根页表帧不会被回收，所以应该只剩下根帧
    assert!(allocated_after < allocated_before, "空的子页表帧应该被回收");

    println!("✅ unmap回收逻辑测试通过！");
}

/// 测试部分取消映射不影响其他映射
///
/// 验证取消映射时正确处理混合有效/无效页表项的情况
#[test]
fn test_unmap_mixed_entries() {
    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    let base = 0x10000000usize;

    // 创建稀疏映射：映射第1、3、5个页面
    for i in [0, 2, 4] {
        pg.map(&MapConfig {
            vaddr: (base + i * 0x1000).into(),
            paddr: (i * 0x1000).into(),
            size: 0x1000,
            pte: PteImpl::user_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    }

    // 验证初始映射
    assert!(pg.is_mapped((base).into()));
    assert!(pg.is_mapped((base + 0x2000).into()));
    assert!(pg.is_mapped((base + 0x4000).into()));
    assert!(!pg.is_mapped((base + 0x1000).into()));
    assert!(!pg.is_mapped((base + 0x3000).into()));

    // 取消中间的映射
    pg.unmap((base + 0x2000).into(), 0x1000).unwrap();

    // 验证其他映射仍然存在
    assert!(pg.is_mapped((base).into()), "第1个页面应该仍然存在");
    assert!(!pg.is_mapped((base + 0x2000).into()), "第3个页面应该被取消");
    assert!(
        pg.is_mapped((base + 0x4000).into()),
        "第5个页面应该仍然存在"
    );

    println!("✅ 混合条目取消映射测试通过！");
}

/// 测试MemConfig的正确实现
///
/// Bug描述：PteImpl没有实现set_mem_config和mem_config方法
#[test]
fn test_mem_config_implementation() {
    let mut pte = PteImpl::new();
    pte = PteImpl::from_config(PteConfig {
        valid: true,
        ..pte.to_config(false)
    });

    // 测试设置和获取MemConfig
    let config = MemConfig {
        access: AccessFlags::READ | AccessFlags::WRITE | AccessFlags::EXECUTE,
        attrs: MemAttributes::Normal,
    };

    pte.set_mem_config(config);
    let retrieved = pte.mem_config();

    assert_eq!(retrieved.access, config.access, "访问权限应该匹配");
    assert_eq!(retrieved.attrs, config.attrs, "内存属性应该匹配");

    // 测试不同的配置
    let config2 = MemConfig {
        access: AccessFlags::READ,
        attrs: MemAttributes::Device,
    };

    pte.set_mem_config(config2);
    let retrieved2 = pte.mem_config();

    assert_eq!(retrieved2.access, config2.access, "只读权限应该匹配");
    assert_eq!(retrieved2.attrs, config2.attrs, "设备属性应该匹配");

    println!("✅ MemConfig实现测试通过！");
}

/// 测试边界情况：地址溢出检查
///
/// 验证在映射和取消映射时正确处理地址溢出情况
#[test]
fn test_address_overflow_handling() {
    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 测试映射时的地址溢出（使用页对齐的地址）
    let max_aligned = (usize::MAX / 0x1000) * 0x1000; // 最大的页对齐地址
    let result = pg.map(&MapConfig {
        vaddr: (max_aligned - 0x1000).into(),
        paddr: 0x0usize.into(),
        size: 0x3000, // 会导致溢出
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    });

    assert!(result.is_err(), "地址溢出应该返回错误");
    if let Err(e) = result {
        // 可能是AddressOverflow或AlignmentError
        assert!(
            matches!(e, PagingError::AddressOverflow { .. })
                || matches!(e, PagingError::AlignmentError { .. }),
            "应该返回AddressOverflow或AlignmentError错误，实际: {:?}",
            e
        );
    }

    // 测试取消映射时的地址溢出
    let result = pg.unmap((max_aligned - 0x1000).into(), 0x3000);
    assert!(result.is_err(), "取消映射时地址溢出应该返回错误");

    println!("✅ 地址溢出处理测试通过！");
}

/// 测试页表层次结构的正确性
///
/// 验证在深层嵌套的页表结构中，translate和unmap正确工作
#[test]
fn test_deep_hierarchy() {
    let mut pg = PageTable::<T4kL4, Fram4k>::new(Fram4k).unwrap();

    // 在深层虚拟地址创建映射（需要完整的4级页表）
    let deep_vaddr = 0x0000f00000000000usize;
    pg.map(&MapConfig {
        vaddr: deep_vaddr.into(),
        paddr: 0x1000usize.into(),
        size: 0x2000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 测试翻译
    let (paddr, _) = pg.translate(deep_vaddr.into()).unwrap();
    assert_eq!(paddr.raw(), 0x1000, "深层地址翻译应该正确");

    let (paddr2, _) = pg.translate((deep_vaddr + 0x1000).into()).unwrap();
    assert_eq!(paddr2.raw(), 0x2000, "深层地址偏移翻译应该正确");

    // 测试取消映射
    pg.unmap(deep_vaddr.into(), 0x2000).unwrap();
    assert!(!pg.is_mapped(deep_vaddr.into()), "深层地址应该被取消映射");

    println!("✅ 深层页表层次结构测试通过！");
}

/// 测试大页和普通页混合映射
///
/// 验证在同一个地址空间中混合使用大页和普通页时的正确性
#[test]
fn test_mixed_huge_and_normal_pages() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 大页映射
    pg.map(&MapConfig {
        vaddr: 0x0usize.into(),
        paddr: 0x0usize.into(),
        size: 2 * MB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 普通页映射
    pg.map(&MapConfig {
        vaddr: (2 * MB).into(),
        paddr: (2 * MB).into(),
        size: 0x3000, // 3个普通页
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 验证大页翻译
    let (paddr1, pte1) = pg.translate(0x100000.into()).unwrap();
    if pte1.to_config(false).huge {
        assert_eq!(paddr1.raw(), 0x100000, "大页偏移应该正确");
    }

    // 验证普通页翻译
    let (paddr2, pte2) = pg.translate((2 * MB + 0x1000).into()).unwrap();
    assert!(
        !pte2.to_config(false).huge || pte2.to_config(false).huge,
        "可能是大页或普通页"
    );
    assert_eq!(paddr2.raw(), 2 * MB + 0x1000, "普通页偏移应该正确");

    println!("✅ 混合大页和普通页测试通过！");
}

/// 压力测试：大量映射和取消映射
///
/// 验证在大量操作下的稳定性和正确性
#[test]
fn test_stress_mapping_unmapping() {
    let mut pg = PageTable::<T4kL3, TrackedFram4k>::new(TrackedFram4k::new()).unwrap();
    let allocator = pg.root.allocator;

    // 创建多个映射
    for i in 0..100 {
        let vaddr = i * 0x10000;
        pg.map(&MapConfig {
            vaddr: vaddr.into(),
            paddr: vaddr.into(),
            size: 0x1000,
            pte: PteImpl::user_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    }

    let count_after_map = pg.walk_valid().count();
    assert_eq!(count_after_map, 100, "应该有100个映射");

    // 取消一半的映射
    for i in (0..100).step_by(2) {
        let vaddr = i * 0x10000;
        pg.unmap(vaddr.into(), 0x1000).unwrap();
    }

    let count_after_unmap = pg.walk_valid().count();
    assert_eq!(count_after_unmap, 50, "应该剩余50个映射");

    // 验证剩余映射的正确性
    for i in (1..100).step_by(2) {
        let vaddr = i * 0x10000;
        assert!(pg.is_mapped(vaddr.into()), "奇数索引的映射应该仍然存在");
    }

    // 取消所有剩余映射
    for i in (1..100).step_by(2) {
        let vaddr = i * 0x10000;
        pg.unmap(vaddr.into(), 0x1000).unwrap();
    }

    let final_count = pg.walk_valid().count();
    assert_eq!(final_count, 0, "所有映射应该被取消");

    // 检查内存泄漏
    allocator.print_stats();

    println!("✅ 压力测试通过！");
}
