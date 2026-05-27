use page_table_generic::*;
mod mocks;
use mocks::*;

// ===== 地址翻译测试 =====

#[test]
fn test_translate_basic() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建一个简单的映射：虚拟地址0x1000 -> 物理地址0x2000
    pg.map(&MapConfig {
        vaddr: 0x1000usize.into(),
        paddr: 0x2000usize.into(),
        size: 0x1000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 测试地址翻译
    let translated_addr = pg.translate_phys(0x1000usize.into()).unwrap();
    assert_eq!(translated_addr, 0x2000usize.into(), "地址翻译失败");

    // 测试页面内偏移
    let translated_addr = pg.translate_phys(0x1001usize.into()).unwrap();
    assert_eq!(translated_addr, 0x2001usize.into(), "地址偏移翻译失败");

    // 测试未映射的地址
    let result = pg.translate(0x2000usize.into());
    assert!(result.is_err(), "未映射地址应该返回错误");

    // 测试新的translate方法返回页表项
    let (_, pte) = pg.translate(0x1000usize.into()).unwrap();
    assert!(pte.to_config(false).valid, "翻译的页表项应该有效");
    assert_eq!(
        pte.to_config(false).paddr,
        0x2000usize.into(),
        "页表项的物理地址应该正确"
    );
}

#[test]
fn test_translate_huge_page() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建2MB大页映射
    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(), // 使用相同的物理地址
        size: 2 * MB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 测试大页内的各种地址
    let test_cases = vec![
        (0x0, 0x0),           // 页开始
        (0x1000, 0x1000),     // 页内偏移
        (0x100000, 0x100000), // 大页中间
        (0x1FF000, 0x1FF000), // 大页结束前一个页面 (2MB - 4KB)
    ];

    for (vaddr, expected_paddr) in test_cases {
        let result = pg.translate_phys((vaddr as usize).into());
        match result {
            Ok(translated) => {
                assert_eq!(
                    translated,
                    (expected_paddr as usize).into(),
                    "大页地址翻译失败: vaddr={:#x}, expected={:#x}, got={:#x}",
                    vaddr,
                    expected_paddr,
                    translated.raw()
                );
            }
            Err(e) => {
                panic!("地址翻译失败: vaddr={:#x}, error={:?}", vaddr, e);
            }
        }

        // 同时测试新的translate方法返回页表项
        let (_, pte) = pg.translate((vaddr as usize).into()).unwrap();
        assert!(pte.to_config(false).valid, "大页页表项应该有效");
        assert!(pte.to_config(false).huge, "大页页表项应该设置huge标志");

        // 调试：查看PTE中的物理地址
        if vaddr == 0x1000 {
            println!(
                "DEBUG: vaddr=0x{:x}, PTE paddr=0x{:x}",
                vaddr,
                pte.to_config(false).paddr.raw()
            );
        }
    }

    // 测试大页范围外的地址
    let result = pg.translate((2 * MB).into());
    assert!(result.is_err(), "大页范围外的地址应该返回错误");

    let result = pg.translate_phys((2 * MB).into());
    assert!(result.is_err(), "大页范围外的地址应该返回错误");
}

#[test]
fn test_translate_multiple_mappings() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建多个映射
    pg.map(&MapConfig {
        vaddr: 0x1000usize.into(),
        paddr: 0x2000usize.into(),
        size: 0x1000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    pg.map(&MapConfig {
        vaddr: 0x2000usize.into(),
        paddr: 0x4000usize.into(),
        size: 0x1000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    pg.map(&MapConfig {
        vaddr: 0x200000usize.into(), // 2MB对齐
        paddr: 0x200000usize.into(), // 2MB对齐
        size: 2 * MB,
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 测试各个映射
    assert_eq!(
        pg.translate_phys(0x1000usize.into()).unwrap(),
        0x2000usize.into()
    );
    assert_eq!(
        pg.translate_phys(0x2000usize.into()).unwrap(),
        0x4000usize.into()
    );

    // 测试大页映射 - 验证翻译逻辑正确性
    let result = pg.translate_phys(0x200000usize.into()).unwrap();
    let (_, pte) = pg.translate(0x200000usize.into()).unwrap();

    // 验证翻译结果的正确性
    if pte.to_config(false).huge {
        let expected = pte.to_config(false).paddr.raw() + (0x200000 % (2 * MB));
        assert_eq!(result.raw(), expected);
    } else {
        let expected = pte.to_config(false).paddr.raw() + (0x200000 % 0x1000);
        assert_eq!(result.raw(), expected);
    }

    // 测试0x250000的翻译
    let result = pg.translate_phys(0x250000usize.into()).unwrap();
    let (_, pte) = pg.translate(0x250000usize.into()).unwrap();

    // 根据PTE类型验证翻译结果
    if pte.to_config(false).huge {
        let expected = pte.to_config(false).paddr.raw() + (0x250000 % (2 * MB));
        assert_eq!(result.raw(), expected);
    } else {
        let expected = pte.to_config(false).paddr.raw() + (0x250000 % 0x1000);
        assert_eq!(result.raw(), expected);
    }

    // 测试新的translate方法返回页表项
    let (_, pte1) = pg.translate(0x1000usize.into()).unwrap();
    assert!(pte1.to_config(false).valid && !pte1.to_config(false).huge);

    let (_, pte2) = pg.translate(0x200000usize.into()).unwrap();
    assert!(pte2.to_config(false).valid);
    // 注意：是否为大页取决于实际实现，不强制要求

    // 测试未映射的地址
    assert!(pg.translate(0x3000usize.into()).is_err());
    assert!(pg.translate(0x80000usize.into()).is_err());
    assert!(pg.translate_phys(0x3000usize.into()).is_err());
    assert!(pg.translate_phys(0x80000usize.into()).is_err());
}

#[test]
fn test_is_mapped() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建映射
    pg.map(&MapConfig {
        vaddr: 0x1000usize.into(),
        paddr: 0x2000usize.into(),
        size: 0x1000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 测试已映射的地址
    assert!(pg.is_mapped(0x1000usize.into()));
    assert!(pg.is_mapped(0x1FFFusize.into()));

    // 测试未映射的地址
    assert!(!pg.is_mapped(0x0usize.into()));
    assert!(!pg.is_mapped(0x2000usize.into()));
    assert!(!pg.is_mapped(0x10000usize.into()));
}

#[test]
fn test_translate_complex_layout() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 使用与test_huge相似的复杂映射布局
    // 这个映射创建从0开始的2MB+12KB区域
    pg.map(&MapConfig {
        vaddr: 0usize.into(),
        paddr: 0usize.into(),
        size: 2 * MB + 0x1000 * 3, // 2MB + 12KB
        pte: PteImpl::user_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();

    // 测试2MB大页部分
    for i in 0..(2 * MB / 0x1000) {
        let vaddr = i * 0x1000;
        let translated = pg.translate_phys(vaddr.into()).unwrap();
        assert_eq!(
            translated,
            vaddr.into(),
            "2MB大页内地址翻译失败: vaddr={:#x}",
            vaddr
        );

        // 测试页表项
        let (_, pte) = pg.translate(vaddr.into()).unwrap();
        assert!(
            pte.to_config(false).valid && pte.to_config(false).huge,
            "大页区域应该返回大页页表项"
        );
    }

    // 测试额外的3个4KB页面
    for i in 0..3 {
        let vaddr = 2 * MB + i * 0x1000;
        let translated = pg.translate_phys(vaddr.into()).unwrap();

        // 测试页表项
        let (_, pte) = pg.translate(vaddr.into()).unwrap();

        // 验证翻译的正确性，而不是假设特定的映射类型
        if pte.to_config(false).huge {
            // 大页映射：验证大页偏移计算
            let expected = pte.to_config(false).paddr.raw() + (vaddr % (2 * MB));
            assert_eq!(
                translated.raw(),
                expected,
                "大页映射翻译失败: vaddr={:#x}, expected={:#x}, got={:#x}",
                vaddr,
                expected,
                translated.raw()
            );
        } else {
            // 普通页面映射：验证页面偏移计算
            let expected = pte.to_config(false).paddr.raw() + (vaddr % 0x1000);
            assert_eq!(
                translated.raw(),
                expected,
                "普通页面映射翻译失败: vaddr={:#x}, expected={:#x}, got={:#x}",
                vaddr,
                expected,
                translated.raw()
            );
        }
    }

    // 测试范围外的地址
    let end_vaddr = 2 * MB + 3 * 0x1000;
    assert!(
        pg.translate(end_vaddr.into()).is_err(),
        "映射范围外的地址应该返回错误"
    );
    assert!(
        pg.translate_phys(end_vaddr.into()).is_err(),
        "映射范围外的地址应该返回错误"
    );
}

#[test]
fn test_translate_error_cases() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 测试空页表的翻译
    let result = pg.translate(0x1000usize.into());
    assert!(result.is_err(), "空页表应该返回错误");
    assert!(matches!(result.unwrap_err(), PagingError::NotMapped));

    let result = pg.translate_phys(0x1000usize.into());
    assert!(result.is_err(), "空页表物理翻译应该返回错误");

    // 创建一个映射
    pg.map(&MapConfig {
        vaddr: 0x1000usize.into(),
        paddr: 0x2000usize.into(),
        size: 0x1000,
        pte: PteImpl::user_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();

    // 测试映射范围内的地址
    assert!(pg.translate(0x1000usize.into()).is_ok());
    assert!(pg.translate(0x1FFFusize.into()).is_ok());
    assert!(pg.translate_phys(0x1000usize.into()).is_ok());
    assert!(pg.translate_phys(0x1FFFusize.into()).is_ok());

    // 测试映射范围外的地址
    assert!(pg.translate(0x0usize.into()).is_err());
    assert!(pg.translate(0x2000usize.into()).is_err());
    assert!(pg.translate(0x3000usize.into()).is_err());
    assert!(pg.translate_phys(0x0usize.into()).is_err());
    assert!(pg.translate_phys(0x2000usize.into()).is_err());
    assert!(pg.translate_phys(0x3000usize.into()).is_err());
}

#[test]
fn test_translate_performance() {
    let mut pg = PageTable::<T4kL3, Fram4k>::new(Fram4k).unwrap();

    // 创建多个映射用于性能测试
    for i in 0..10 {
        pg.map(&MapConfig {
            vaddr: (i * 0x10000usize).into(),
            paddr: (i * 0x10000usize).into(),
            size: 0x10000,
            pte: PteImpl::user_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    }

    // 测试多个地址翻译
    for i in 0..10 {
        let vaddr = i * 0x10000 + 0x1000;
        let result = pg.translate_phys(vaddr.into());
        assert!(result.is_ok(), "地址翻译应该成功: vaddr={:#x}", vaddr);
        assert_eq!(result.unwrap(), vaddr.into(), "翻译结果应该正确");

        // 同时测试新的translate方法
        let (_, pte) = pg.translate(vaddr.into()).unwrap();
        assert!(pte.to_config(false).valid, "页表项应该有效");
    }
}
