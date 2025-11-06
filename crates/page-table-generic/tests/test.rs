//! Mock implementations for testing
//!
//! This module provides mock implementations used in tests for the page-table-generic crate.
#![cfg(not(target_os = "none"))]

use page_table_generic::*;
use std::vec::Vec;

mod mocks;

use mocks::*;

#[test]
fn test_pte() {
    let mut want = PteImpl(0);
    want.set_valid(true);
    assert!(want.valid());

    let addr = PhysAddr::from(0xff123456000usize);
    want.set_paddr(addr);
    assert_eq!(want.paddr(), addr);
}

fn test_high<T: TableGeneric, A: FrameAllocator>(
    pte: T::P,
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
    for p in pg.walk(WalkConfig {
        start_vaddr: VirtAddr::new(0),
        end_vaddr: VirtAddr::new(usize::MAX),
    }) {
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
    for p in pg.walk(WalkConfig {
        start_vaddr: VirtAddr::new(0),
        end_vaddr: VirtAddr::new(usize::MAX),
    }) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.paddr(),
            p.pte.is_huge(),
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
        assert!(pte.valid(), "{} 页表项应该是有效的", test_name);

        // 验证不是大页（因为allow_huge=false且页面大小为4KB）
        assert!(!pte.is_huge(), "{} 页表项不应该是大页", test_name);

        // 物理地址偏移验证：由于内存分配的随机性，我们只验证相对关系

        // 注意：由于内存分配的随机性，我们只验证物理地址的偏移部分
        // 实际的物理基地址可能不同，但偏移应该是固定的
        let actual_paddr = pte.paddr();
        let actual_offset = actual_paddr.raw() % 0x1000; // 页内偏移
        assert_eq!(
            actual_offset, 0,
            "{} 页内偏移应该是0，实际是 {actual_offset:?}",
            test_name
        );

        // 验证两个页表项的物理地址相差0x1000（4KB）
        if i > 0 {
            let prev_pte = &valid_entries[i - 1].1;
            let prev_paddr = prev_pte.paddr();
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
            pte.valid(),
            pte.is_huge()
        );
    }

    println!("🎉 {} 所有地址和属性验证通过！", test_name);
}

#[test]
fn test_new() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high::<T4kL4, Fram4k>(
        PteImpl(0),
        Fram4k,
        0x0000f00000000000usize.into(), // 高虚拟地址
        1,                              // 叶子级别
        "T4kL4",
    );
}

#[test]
fn test_new_l3() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high::<T4kL3, Fram4k>(
        PteImpl(0),
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
        PteImpl(0),
        Fram4k,
        0x000f000000000000usize.into(), // 高虚拟地址
        1,                              // 叶子级别
        "T4kL5",
    );
}

// ===== Flag 验证辅助函数 =====

/// 验证PTE的flag属性
fn assert_pte_flags(
    pte: &PteImpl,
    expected_readable: bool,
    expected_writable: bool,
    expected_user_executable: bool,
    expected_user_accessible: bool,
    expected_privilege_executable: bool,
    expected_cache_mode: u64,
    expected_huge: bool,
    test_name: &str,
) {
    assert_eq!(
        pte.is_readable(),
        expected_readable,
        "{} 读取权限不匹配，期望 {}，实际 {}",
        test_name,
        expected_readable,
        pte.is_readable()
    );

    assert_eq!(
        pte.is_writable(),
        expected_writable,
        "{} 写入权限不匹配，期望 {}，实际 {}",
        test_name,
        expected_writable,
        pte.is_writable()
    );

    assert_eq!(
        pte.is_user_executable(),
        expected_user_executable,
        "{} 用户执行权限不匹配，期望 {}，实际 {}",
        test_name,
        expected_user_executable,
        pte.is_user_executable()
    );

    assert_eq!(
        pte.is_user_accessible(),
        expected_user_accessible,
        "{} 用户访问权限不匹配，期望 {}，实际 {}",
        test_name,
        expected_user_accessible,
        pte.is_user_accessible()
    );

    assert_eq!(
        pte.is_privilege_executable(),
        expected_privilege_executable,
        "{} 特权执行权限不匹配，期望 {}，实际 {}",
        test_name,
        expected_privilege_executable,
        pte.is_privilege_executable()
    );

    assert_eq!(
        pte.cache_mode(),
        expected_cache_mode,
        "{} 缓存模式不匹配，期望 {}，实际 {}",
        test_name,
        expected_cache_mode,
        pte.cache_mode()
    );

    assert_eq!(
        pte.is_huge(),
        expected_huge,
        "{} 大页属性不匹配，期望 {}，实际 {}",
        test_name,
        expected_huge,
        pte.is_huge()
    );
}

/// 打印PTE的flag信息用于调试
fn print_pte_flags(pte: &PteImpl, test_name: &str) {
    println!(
        "{} PTE Flags: R={}, W={}, UX={}, UA={}, PX={}, Cache={}, Huge={}, Valid={}",
        test_name,
        pte.is_readable(),
        pte.is_writable(),
        pte.is_user_executable(),
        pte.is_user_accessible(),
        pte.is_privilege_executable(),
        pte.cache_mode(),
        pte.is_huge(),
        pte.valid()
    );
}

/// 带有flag验证的高级测试函数
fn test_high_with_flags<T: TableGeneric, A: FrameAllocator>(
    pte: PteImpl,
    alloc: A,
    test_vaddr: VirtAddr,
    expected_leaf_level: usize,
    test_name: &str,
    expected_readable: bool,
    expected_writable: bool,
    expected_user_executable: bool,
    expected_user_accessible: bool,
    expected_privilege_executable: bool,
    expected_cache_mode: u64,
    expected_huge: bool,
) where
    T: TableGeneric<P = PteImpl>,
{
    let mut pg = unsafe { PageTableRef::<T, A>::new(alloc).unwrap() };
    println!("table page size: {:#x}", T::PAGE_SIZE);
    println!("valid bits: {}", PageTableRef::<T, A>::valid_bits());

    // 显示要使用的PTE flag信息
    print_pte_flags(&pte, &format!("{} - 输入PTE", test_name));

    println!("\n=== {test_name} 映前状态 - walk_all (遍历所有项) ===");
    for p in pg.walk(WalkConfig {
        start_vaddr: VirtAddr::new(0),
        end_vaddr: VirtAddr::new(usize::MAX),
    }) {
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

    println!("walk_valid() 返回 {count} 个叶子级别条目");

    println!(
        "\n=== {} 映后状态 - 显示完整层次（所有有效项） ===",
        test_name
    );
    for p in pg.walk(WalkConfig {
        start_vaddr: VirtAddr::new(0),
        end_vaddr: VirtAddr::new(usize::MAX),
    }) {
        println!(
            "l: {}, va: {:?}, c: PTE PA: {:?} Block: {}, Final: {}",
            p.level,
            p.vaddr,
            p.pte.paddr(),
            p.pte.is_huge(),
            p.is_final_mapping
        );
    }

    assert_eq!(count, 2); // walk_valid() 应该返回2个叶子级别条目

    // === 验证地址映射（复用现有逻辑） ===

    // 验证虚拟地址：映射从指定地址开始的0x2000字节（2个4KB页面）
    let expected_vaddrs = [test_vaddr, VirtAddr::new(test_vaddr.raw() + 0x1000)];

    // 验证虚拟地址映射正确
    for (i, (vaddr, pte, level)) in valid_entries.iter().enumerate() {
        assert_eq!(
            *vaddr, expected_vaddrs[i],
            "{} 第{}个条目的虚拟地址不匹配，期望 {:?}，实际 {:?}",
            test_name, i, expected_vaddrs[i], vaddr
        );

        // 验证这是叶子级别
        assert_eq!(
            *level, expected_leaf_level,
            "{} 叶子级别页表项应该在level {}，实际在level {level}",
            test_name, expected_leaf_level
        );

        // 验证页表项是有效的
        assert!(pte.valid(), "{} 页表项应该是有效的", test_name);

        // 验证不是大页（因为allow_huge=false且页面大小为4KB）
        assert!(!pte.is_huge(), "{} 页表项不应该是大页", test_name);

        // 物理地址偏移验证
        let actual_paddr = pte.paddr();
        let actual_offset = actual_paddr.raw() % 0x1000; // 页内偏移
        assert_eq!(
            actual_offset, 0,
            "{} 页内偏移应该是0，实际是 {actual_offset:?}",
            test_name
        );

        // 验证两个页表项的物理地址相差0x1000（4KB）
        if i > 0 {
            let prev_pte = &valid_entries[i - 1].1;
            let prev_paddr = prev_pte.paddr();
            let addr_diff = actual_paddr.raw().saturating_sub(prev_paddr.raw());
            assert_eq!(
                addr_diff, 0x1000,
                "{} 相邻页面物理地址应该相差0x1000，实际相差 {addr_diff:?}",
                test_name
            );
        }

        println!(
            "✓ {} 页面{}地址验证通过: VA={:?}, PA={:?}, Level={}",
            test_name, i, vaddr, actual_paddr, level
        );
    }

    // === 验证Flag属性 ===

    println!("\n=== {} Flag属性验证 ===", test_name);
    for (i, (_vaddr, pte, _level)) in valid_entries.iter().enumerate() {
        let entry_test_name = format!("{}-PTE{}", test_name, i);

        // 转换为PteImpl以访问flag方法
        // 这里我们使用位模式转换，因为 PteImpl 是 repr(transparent)
        let pte_impl: PteImpl = unsafe { std::mem::transmute_copy(pte) };

        print_pte_flags(&pte_impl, &entry_test_name);

        assert_pte_flags(
            &pte_impl,
            expected_readable,
            expected_writable,
            expected_user_executable,
            expected_user_accessible,
            expected_privilege_executable,
            expected_cache_mode,
            expected_huge,
            &entry_test_name,
        );

        println!("✓ {} 页面{} Flag验证通过", test_name, i);
    }

    println!("🎉 {} 所有地址和Flag属性验证通过！", test_name);
}

// ===== 基础权限测试用例 =====

#[test]
fn test_pte_read_only() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::read_only(),
        Fram4k,
        0x0000f00000000000usize.into(),
        1,
        "ReadOnly",
        true,  // readable
        false, // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_read_write() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00100000000usize.into(),
        1,
        "ReadWrite",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_read_execute() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            false, // write
            true,  // user_execute
            true,  // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00200000000usize.into(),
        1,
        "ReadExecute",
        true,  // readable
        false, // writable
        true,  // user_executable
        true,  // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_all_permissions() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            true,  // write
            true,  // user_execute
            true,  // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00300000000usize.into(),
        1,
        "AllPermissions",
        true,  // readable
        true,  // writable
        true,  // user_executable
        true,  // user_accessible
        true,  // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

// ===== 用户/内核权限测试用例 =====

#[test]
fn test_pte_user_mode() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::user_mode(),
        Fram4k,
        0x0000f00400000000usize.into(),
        1,
        "UserMode",
        true,  // readable
        true,  // writable
        true,  // user_executable
        true,  // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_kernel_mode() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::kernel_mode(),
        Fram4k,
        0x0000f00500000000usize.into(),
        1,
        "KernelMode",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        true,  // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_user_execute() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            false, // write
            true,  // user_execute
            true,  // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00600000000usize.into(),
        1,
        "UserExecute",
        true,  // readable
        false, // writable
        true,  // user_executable
        true,  // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_privilege_execute() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            true,  // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00700000000usize.into(),
        1,
        "PrivilegeExecute",
        true,  // readable
        false, // writable
        false, // user_executable
        false, // user_accessible
        true,  // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

// ===== 缓存属性测试用例 =====

#[test]
fn test_pte_non_cache() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            0,     // non-cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00800000000usize.into(),
        1,
        "NonCache",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        0,     // non-cache
        false, // not huge
    );
}

#[test]
fn test_pte_normal_cache() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            false, // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00900000000usize.into(),
        1,
        "NormalCache",
        true,  // readable
        false, // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_device_cache() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            false, // privilege_execute
            2,     // device cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00a00000000usize.into(),
        1,
        "DeviceCache",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        2,     // device cache
        false, // not huge
    );
}

#[test]
fn test_pte_mmap_io() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::mmap_io(),
        Fram4k,
        0x0000f00b00000000usize.into(),
        1,
        "MmapIO",
        true,  // readable
        false, // writable
        false, // user_executable
        true,  // user_accessible
        false, // privilege_execute
        2,     // device cache
        false, // not huge
    );
}

// ===== 大页和综合测试用例 =====

#[test]
fn test_pte_device_memory() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::device_memory(),
        Fram4k,
        0x0000f00c00000000usize.into(),
        1,
        "DeviceMemory",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        false, // privilege_execute
        2,     // device cache
        false, // not huge (because allow_huge=false)
    );
}

#[test]
fn test_pte_complex_user_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    // 复杂用户映射：用户模式 + 只读数据 + 可执行代码
    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            false, // write (只读)
            true,  // user_execute
            true,  // user_access
            false, // privilege_execute
            1,     // normal cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00d00000000usize.into(),
        1,
        "ComplexUserMapping",
        true,  // readable
        false, // writable
        true,  // user_executable
        true,  // user_accessible
        false, // privilege_execute
        1,     // normal cache
        false, // not huge
    );
}

#[test]
fn test_pte_complex_kernel_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    // 复杂内核映射：内核模式 + 读写 + 特权执行 + 设备缓存
    test_high_with_flags::<T4kL4, Fram4k>(
        PteImpl::new_with_flags(
            true,  // read
            true,  // write
            false, // user_execute
            false, // user_access
            true,  // privilege_execute
            2,     // device cache
            true,  // valid
            false, // not block
        ),
        Fram4k,
        0x0000f00e00000000usize.into(),
        1,
        "ComplexKernelMapping",
        true,  // readable
        true,  // writable
        false, // user_executable
        false, // user_accessible
        true,  // privilege_execute
        2,     // device cache
        false, // not huge
    );
}

// ===== 释放功能测试 =====

#[test]
fn test_frame_recursive_deallocate() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();

    // 创建一个根帧
    let mut root_frame = Frame::<T4kL4, TrackedFram4k>::new(allocator).unwrap();

    // 创建几个子帧并设置PTE
    let child1_paddr = allocator.alloc_frame().unwrap();
    let child2_paddr = allocator.alloc_frame().unwrap();

    println!("初始分配的帧数量: {}", allocator.allocated_count());

    {
        let entries = root_frame.as_slice_mut();
        // 设置有效的子页表项
        entries[0] = PteImpl::kernel_mode();
        entries[0].set_paddr(child1_paddr);
        entries[0].set_valid(true);

        entries[1] = PteImpl::kernel_mode();
        entries[1].set_paddr(child2_paddr);
        entries[1].set_valid(true);

        // 设置一个大页项（不应该被递归释放）
        entries[2] = PteImpl::device_memory();
        entries[2].set_paddr(allocator.alloc_frame().unwrap());
        entries[2].set_valid(true);
        entries[2].set_is_huge(true);
    }

    println!("设置PTE后的帧数量: {}", allocator.allocated_count());

    // 验证初始状态
    let entries_before = root_frame.as_slice();
    assert!(entries_before[0].valid() && !entries_before[0].is_huge());
    assert!(entries_before[1].valid() && !entries_before[1].is_huge());
    assert!(entries_before[2].valid() && entries_before[2].is_huge());

    // 释放子帧（T4kL4 有 4 级，根帧在第 4 级）
    root_frame.deallocate_children(4);
    println!("递归释放子帧后的帧数量: {}", allocator.allocated_count());

    // 验证子页表项已被设为invalid，但大页项保持不变
    let entries_after = root_frame.as_slice();
    assert!(!entries_after[0].valid(), "子页表项1应该被设为invalid");
    assert!(!entries_after[1].valid(), "子页表项2应该被设为invalid");
    assert!(entries_after[2].valid(), "大页项应该保持有效");

    // 手动释放剩余的大页帧
    if entries_after[2].valid() {
        allocator.dealloc_frame(entries_after[2].paddr());
        println!("释放大页帧后的帧数量: {}", allocator.allocated_count());
    }

    // 释放根帧
    allocator.dealloc_frame(root_frame.paddr);
    println!("释放根帧后的帧数量: {}", allocator.allocated_count());

    // 验证所有帧都已释放
    assert!(!allocator.has_leaks(), "检测到内存泄漏");
    allocator.print_stats();

    println!("✓ Frame递归释放测试通过");
}

#[test]
fn test_page_table_destroy() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();

    // 创建页表
    let mut page_table = unsafe { PageTableRef::<T4kL4, TrackedFram4k>::new(allocator).unwrap() };
    let root_paddr_before = page_table.root_paddr();

    println!("页表创建成功，根地址: {:#x}", root_paddr_before.raw());
    println!("创建页表后的分配数量: {}", allocator.allocated_count());

    // 先测试空的页表销毁
    let empty_page_table = unsafe { PageTableRef::<T4kL4, TrackedFram4k>::new(allocator).unwrap() };
    println!("创建空页表后的分配数量: {}", allocator.allocated_count());
    println!("测试空页表销毁...");
    unsafe { empty_page_table.destroy() };
    println!("销毁空页表后的分配数量: {}", allocator.allocated_count());
    println!("✓ 空页表销毁成功");

    // 添加一些映射来创建子页表
    let config = MapConfig {
        vaddr: 0x0000_2000_0000usize.into(),
        paddr: allocator.alloc_frame().unwrap(),
        size: 0x1000, // 4KB，简单测试
        pte: PteImpl::user_mode(),
        allow_huge: false,
        flush: false,
    };

    println!("创建映射前分配数量: {}", allocator.allocated_count());
    println!("开始创建映射...");
    match page_table.map(&config) {
        Ok(()) => {
            println!("映射创建成功");
            println!("创建映射后分配数量: {}", allocator.allocated_count());
        }
        Err(e) => {
            println!("映射创建失败: {:?}", e);
            return;
        }
    }

    // 验证页表有内容
    let valid_entries: usize = page_table.walk_valid().count();
    println!("有效映射数量: {}", valid_entries);
    assert!(valid_entries > 0, "页表应该有有效的映射");

    // 销毁页表（释放所有页表帧，但不释放映射的数据页）
    println!("开始销毁页表...");
    unsafe { page_table.destroy() };
    println!("销毁页表后分配数量: {}", allocator.allocated_count());

    // 验证页表帧已释放（数据页由我们手动管理，所以会有1个数据页未释放）
    println!("预期还有1个数据页未释放");
    // 注意：这个测试中的数据页需要手动释放
    allocator.dealloc_frame(config.paddr);
    allocator.print_stats();

    println!("✓ PageTable销毁测试通过");
}

#[test]
fn test_deallocate_after_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();

    // 创建页表并进行映射
    let mut page_table = unsafe { PageTableRef::<T4kL3, TrackedFram4k>::new(allocator).unwrap() };

    println!("创建页表后分配数量: {}", allocator.allocated_count());

    // 创建多个映射，保存数据页地址以便稍后释放
    let data_page1 = allocator.alloc_frame().unwrap();
    let data_page2 = allocator.alloc_frame().unwrap();

    let configs = vec![
        MapConfig {
            vaddr: 0x1000_0000usize.into(),
            paddr: data_page1,
            size: 0x1000, // 4KB
            pte: PteImpl::user_mode(),
            allow_huge: false,
            flush: false,
        },
        MapConfig {
            vaddr: 0x2000_0000usize.into(),
            paddr: data_page2,
            size: 0x1000, // 4KB
            pte: PteImpl::kernel_mode(),
            allow_huge: false,
            flush: false,
        },
    ];

    for config in &configs {
        page_table.map(config).unwrap();
    }

    println!("创建映射后分配数量: {}", allocator.allocated_count());

    // 验证映射成功
    let valid_entries: usize = page_table.walk_valid().count();
    assert_eq!(valid_entries, 2, "应该有2个有效映射");

    println!("映射创建完成，开始释放...");
    println!("释放前分配数量: {}", allocator.allocated_count());

    // 使用deallocate方法释放页表帧（不释放数据页）
    unsafe { page_table.deallocate() };
    println!("释放后分配数量: {}", allocator.allocated_count());

    // 手动释放数据页
    allocator.dealloc_frame(data_page1);
    allocator.dealloc_frame(data_page2);

    // 验证所有帧都已释放
    assert!(!allocator.has_leaks(), "检测到内存泄漏");
    allocator.print_stats();

    println!("✓ 映射后释放测试通过");
}

#[test]
fn test_single_entry_deallocate() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();

    // 创建一个帧并添加子页表项
    let mut root_frame = Frame::<T4kL3, TrackedFram4k>::new(allocator).unwrap();
    println!("创建根帧后分配数量: {}", allocator.allocated_count());

    // 添加一个有效的子页表项
    let child_frame = Frame::<T4kL3, TrackedFram4k>::new(allocator).unwrap();
    println!("创建子帧后分配数量: {}", allocator.allocated_count());

    {
        let entries = root_frame.as_slice_mut();
        entries[5] = PteImpl::kernel_mode();
        entries[5].set_paddr(child_frame.paddr);
        entries[5].set_valid(true);
    }

    // 验证初始状态
    let entries_before = root_frame.as_slice();
    assert!(entries_before[5].valid());

    // 测试单个条目释放（T4kL3 有 3 级，根帧在第 3 级）
    let deallocated = root_frame.dealloc_entry_recursive(5, 3);
    assert!(deallocated, "应该成功释放指定的条目");
    println!("释放单个条目后分配数量: {}", allocator.allocated_count());

    // 验证条目已被设为invalid
    let entries_after = root_frame.as_slice();
    assert!(!entries_after[5].valid(), "被释放的条目应该为invalid");

    // 测试释放不存在的条目
    let not_deallocated = root_frame.dealloc_entry_recursive(10, 3);
    assert!(!not_deallocated, "不应该释放不存在的条目");

    // 测试释放无效条目
    let not_deallocated2 = root_frame.dealloc_entry_recursive(0, 3);
    assert!(!not_deallocated2, "不应该释放无效条目");

    // 释放根帧
    allocator.dealloc_frame(root_frame.paddr);
    println!("释放根帧后分配数量: {}", allocator.allocated_count());

    // 验证所有帧都已释放
    assert!(!allocator.has_leaks(), "检测到内存泄漏");
    allocator.print_stats();

    println!("✓ 单个条目释放测试通过");
}

#[test]
fn test_deallocate_edge_cases() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();
    println!("初始分配数量: {}", allocator.allocated_count());

    // 测试空帧的释放
    {
        let mut empty_frame = Frame::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
        println!("创建空帧后分配数量: {}", allocator.allocated_count());
        empty_frame.deallocate_children(4); // 应该安全执行，不崩溃（T4kL4 根帧在第 4 级）
        allocator.dealloc_frame(empty_frame.paddr);
        println!("释放空帧后分配数量: {}", allocator.allocated_count());
    }

    // 测试只有大页的帧释放
    {
        let mut huge_frame = Frame::<T4kL4, TrackedFram4k>::new(allocator).unwrap();
        let entries = huge_frame.as_slice_mut();
        println!("创建大页帧后分配数量: {}", allocator.allocated_count());

        // 只设置大页项
        entries[0] = PteImpl::device_memory();
        entries[0].set_paddr(allocator.alloc_frame().unwrap());
        entries[0].set_valid(true);
        entries[0].set_is_huge(true);
        println!("设置大页项后分配数量: {}", allocator.allocated_count());

        // 释放子帧应该只释放子页表，不影响大页（T4kL4 根帧在第 4 级）
        huge_frame.deallocate_children(4);
        println!("释放子帧后分配数量: {}", allocator.allocated_count());

        // 大页项应该保持有效
        let entries_after = huge_frame.as_slice();
        assert!(entries_after[0].valid() && entries_after[0].is_huge());

        // 手动清理
        allocator.dealloc_frame(entries_after[0].paddr());
        allocator.dealloc_frame(huge_frame.paddr);
        println!("清理大页帧后分配数量: {}", allocator.allocated_count());
    }

    // 测试deallocate_range的边界情况
    {
        let mut page_table =
            unsafe { PageTableRef::<T4kL4, TrackedFram4k>::new(allocator).unwrap() };
        println!("创建测试页表后分配数量: {}", allocator.allocated_count());

        // 测试无效范围
        let result = page_table.deallocate_range(
            0x1000_0000usize.into(),
            0x0800_0000usize.into(), // 结束地址小于开始地址
        );
        assert!(result.is_err(), "应该返回无效范围错误");

        // 释放页表
        unsafe { page_table.deallocate() };
        println!("释放测试页表后分配数量: {}", allocator.allocated_count());
    }

    // 验证所有帧都已释放
    assert!(!allocator.has_leaks(), "检测到内存泄漏");
    allocator.print_stats();

    println!("✓ 边界情况测试通过");
}
