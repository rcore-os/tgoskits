mod mocks;
use mocks::*;
use page_table_generic::*;

#[test]
fn test_deallocate() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let allocator = TrackedFram4k::new();

    // 创建页表并进行映射
    let mut page_table = PageTable::<T4kL3, TrackedFram4k>::new(allocator).unwrap();

    println!("创建页表后分配数量: {}", allocator.allocated_count());

    let configs = vec![
        MapConfig {
            vaddr: 0x1000_0000usize.into(),
            paddr: 0x1000_0000usize.into(),
            size: GB + 2 * MB + 0x1000 * 3,
            // size: GB,
            pte: PteImpl::user_mode_config(),
            allow_huge: true,
            flush: false,
        },
        MapConfig {
            vaddr: 0x0usize.into(),
            paddr: 0x0usize.into(),
            size: 0x2000 + 2 * MB,
            pte: PteImpl::kernel_mode_config(),
            allow_huge: true,
            flush: false,
        },
    ];

    for config in &configs {
        page_table.map(config).unwrap();
    }

    println!("创建映射后分配数量: {}", allocator.allocated_count());

    // // 验证映射成功
    // let valid_entries: usize = page_table.walk_valid().count();

    // assert_eq!(valid_entries, 2, "应该有2个有效映射");

    println!("映射创建完成，开始释放...");
    println!("释放前分配数量: {}", allocator.allocated_count());

    drop(page_table);

    println!("释放后分配数量: {}", allocator.allocated_count());

    // 验证所有帧都已释放
    assert!(!allocator.has_leaks(), "检测到内存泄漏");
    allocator.print_stats();

    println!("✓ 映射后释放测试通过");
}
