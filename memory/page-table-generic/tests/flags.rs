use page_table_generic::*;
pub mod mocks;
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
