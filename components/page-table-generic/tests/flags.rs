use page_table_generic::*;
mod mocks;
use mocks::*;

// ===== 页表标志位测试 =====

#[test]
fn test_pte() {
    let pte = PteImpl::new();
    println!("PTE: {:?}", pte);
    assert!(!pte.to_config(false).valid);
    assert!(!pte.to_config(false).huge);
    println!("✓ Empty PTE test passed");
}

#[test]
fn test_pte_read_only() {
    let pte = PteImpl::read_only();
    assert!(pte.to_config(false).valid);
    assert!(pte.is_readable());
    assert!(!pte.is_writable());
    assert!(!pte.is_user_executable());
    assert!(!pte.is_user_accessible());
    assert!(!pte.is_privilege_executable());
    assert_eq!(pte.cache_mode(), 1); // normal cache
    assert!(!pte.to_config(false).huge);
    println!("✓ ReadOnly PTE test passed");
}

#[test]
fn test_pte_user_mode() {
    let pte = PteImpl::user_mode();
    assert!(pte.to_config(false).valid);
    assert!(pte.is_readable());
    assert!(pte.is_writable());
    assert!(pte.is_user_executable());
    assert!(pte.is_user_accessible());
    assert!(!pte.is_privilege_executable());
    assert_eq!(pte.cache_mode(), 1); // normal cache
    assert!(!pte.to_config(false).huge);
    println!("✓ UserMode PTE test passed");
}

#[test]
fn test_pte_kernel_mode() {
    let pte = PteImpl::kernel_mode();
    assert!(pte.to_config(false).valid);
    assert!(pte.is_readable());
    assert!(pte.is_writable());
    assert!(!pte.is_user_executable());
    assert!(!pte.is_user_accessible());
    assert!(pte.is_privilege_executable());
    assert_eq!(pte.cache_mode(), 1); // normal cache
    assert!(!pte.to_config(false).huge);
    println!("✓ KernelMode PTE test passed");
}

#[test]
fn test_pte_device_memory() {
    let pte = PteImpl::device_memory();
    assert!(pte.to_config(false).valid);
    assert!(pte.is_readable());
    assert!(pte.is_writable());
    assert!(!pte.is_user_executable());
    assert!(!pte.is_user_accessible());
    assert!(!pte.is_privilege_executable());
    assert_eq!(pte.cache_mode(), 2); // device cache
    assert!(pte.to_config(false).huge);
    println!("✓ DeviceMemory PTE test passed");
}

#[test]
fn test_pte_complex_mapping() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 测试复杂用户映射 - 使用2MB对齐的地址以确保可以创建大页
    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(), // 两个地址都2MB对齐
        size: 2 * MB,
        pte: PteImpl::complex_user_mapping_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 验证映射成功
    assert!(pg.is_mapped(0usize.into()));
    assert_eq!(
        pg.translate_phys(0usize.into()).unwrap(),
        0usize.into() // 物理地址也是0
    );

    // 测试新的translate方法返回页表项
    let (_, pte) = pg.translate(0usize.into()).unwrap();
    assert!(pte.to_config(false).valid);
    // 注意：是否为大页取决于实际的映射实现，可能是大页也可能是普通页
    // 如果是大页，验证其属性
    if pte.to_config(false).huge {
        println!("✓ 创建了大页映射");
    } else {
        println!("✓ 创建了普通页映射");
    }

    println!("✓ Complex mapping test passed");
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
        pte.to_config(false).huge,
        expected_huge,
        "{} 大页属性不匹配，期望 {}，实际 {}",
        test_name,
        expected_huge,
        pte.to_config(false).huge
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
        pte.to_config(false).huge,
        pte.to_config(false).valid
    );
}

/// 带有flag验证的高级测试函数
fn test_high_with_flags<T: TableMeta, A: FrameAllocator>(
    pte: PteConfig,
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
    T: TableMeta<P = PteImpl>,
{
    let mut pg = unsafe { PageTableRef::<T, A>::new(alloc).unwrap() };
    println!("table page size: {:#x}", T::PAGE_SIZE);
    println!("valid bits: {}", PageTableRef::<T, A>::valid_bits());

    // 显示要使用的PTE flag信息
    let pte_impl = T::P::from_config(pte);
    print_pte_flags(&pte_impl, &format!("{} - 输入PTE", test_name));

    println!("\n=== {test_name} 映前状态 - walk_all (遍历所有项) ===");
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

        // 物理地址偏移验证
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
