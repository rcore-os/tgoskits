//! LoongArch64 页表映射场景模拟测试（简化版）
//!
//! 模拟 LoongArch64 启动时的内核代码段映射场景

#![cfg(not(target_os = "none"))]

use page_table_generic::*;

mod mocks;

/// LoongArch64 Generic 配置的 Mock 实现
#[derive(Debug, Clone, Copy)]
pub struct LoongArch64Generic;

impl TableGeneric for LoongArch64Generic {
    type P = mocks::PteImpl;

    const PAGE_SIZE: usize = 0x1000; // 4KB

    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 2; // PUD (level 2) 支持 2MB 巨页

    fn flush(vaddr: Option<VirtAddr>) {
        let _ = vaddr;
    }
}

const MB: usize = 1024 * 1024;

/// 模拟 __kimage_va 宏
const fn __kimage_va_sim(phys: usize) -> usize {
    const KERNEL_VIRT_OFFSET: usize = 0x9000_0000_0000;
    KERNEL_VIRT_OFFSET + phys
}

/// 测试 LoongArch64 内核代码段映射
#[test]
fn test_loongarch64_kernel_code_mapping() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Debug)
        .try_init();

    let mut pg = PageTable::<LoongArch64Generic, mocks::Fram4k>::new(mocks::Fram4k).unwrap();

    let kernel_phys_start = 0x2000_0000usize;
    let kernel_virt_start = __kimage_va_sim(kernel_phys_start);
    let kernel_size = 2 * MB;

    println!("=== LoongArch64 内核代码映射测试 ===");
    println!("物理起始: {:#x}", kernel_phys_start);
    println!("虚拟起始: {:#x}", kernel_virt_start);
    println!("大小: {:#x} ({} MB)", kernel_size, kernel_size / MB);

    // 创建一个基本的页表项配置
    let result = pg.map(&MapConfig {
        vaddr: kernel_virt_start.into(),
        paddr: kernel_phys_start.into(),
        size: kernel_size,
        pte: mocks::PteImpl::kernel_mode_config(),
        allow_huge: true,
        flush: false,
    });

    assert!(result.is_ok(), "内核代码映射应该成功: {:?}", result.err());

    println!("✓ 映射成功");

    // 验证起始地址可翻译
    let result = pg.translate(kernel_virt_start.into());
    assert!(result.is_ok(), "起始地址应该可翻译");

    let (translated_paddr, pte) = result.unwrap();
    println!(
        "起始地址翻译: VA={:#x} -> PA={:#x}, Huge={}",
        kernel_virt_start,
        translated_paddr.raw(),
        pte.to_config(false).huge
    );

    assert_eq!(
        translated_paddr.raw(),
        kernel_phys_start,
        "物理地址应该匹配"
    );

    println!("🎉 LoongArch64 内核代码映射测试通过！");
}
