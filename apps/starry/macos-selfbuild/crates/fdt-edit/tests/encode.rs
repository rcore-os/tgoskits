//! FDT 编码测试

use dtb_file::*;
use fdt_edit::*;

/// 测试空 FDT 的编码
#[test]
fn test_encode_empty_fdt() {
    let fdt = Fdt::new();
    let encoded = fdt.encode();

    // 编码后的数据不应为空
    assert!(!encoded.is_empty());

    // 至少应该包含 header (40 bytes)
    assert!(encoded.len() >= 40);

    // 应该能被成功解析
    let parsed = Fdt::from_bytes(&encoded);
    assert!(parsed.is_ok());
}

/// 测试带有属性的 FDT 编码
#[test]
fn test_encode_with_properties() {
    let mut fdt = Fdt::new();

    // 添加一些属性到根节点
    let root_id = fdt.root_id();
    let node = fdt.node_mut(root_id).unwrap();
    node.set_property(crate::Property::new(
        "#address-cells",
        vec![0x00, 0x00, 0x00, 0x02],
    ));
    node.set_property(crate::Property::new(
        "#size-cells",
        vec![0x00, 0x00, 0x00, 0x01],
    ));
    node.set_property(crate::Property::new("model", {
        let mut v = b"Test Device".to_vec();
        v.push(0);
        v
    }));

    let encoded = fdt.encode();

    // 解析并验证
    let parsed = Fdt::from_bytes(&encoded).unwrap();
    let root = parsed.get_by_path("/").unwrap();
    let node_ref = root.as_node();

    // 验证属性
    assert_eq!(node_ref.address_cells(), Some(2));
    assert_eq!(node_ref.size_cells(), Some(1));
    assert_eq!(
        node_ref.get_property("model").unwrap().as_str(),
        Some("Test Device")
    );
}

/// 测试带有子节点的 FDT 编码
#[test]
fn test_encode_with_children() {
    let mut fdt = Fdt::new();

    // 添加子节点
    let root_id = fdt.root_id();
    let mut soc = crate::Node::new("soc");
    soc.set_property(crate::Property::new(
        "#address-cells",
        vec![0x00, 0x00, 0x00, 0x02],
    ));
    soc.set_property(crate::Property::new(
        "#size-cells",
        vec![0x00, 0x00, 0x00, 0x02],
    ));
    let soc_id = fdt.add_node(root_id, soc);

    let mut uart = crate::Node::new("uart@1000");
    uart.set_property(crate::Property::new("reg", {
        let v = vec![0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00];
        v
    }));
    uart.set_property(crate::Property::new("compatible", {
        let mut v = b"test,uart".to_vec();
        v.push(0);
        v
    }));
    fdt.add_node(soc_id, uart);

    let encoded = fdt.encode();

    // 解析并验证
    let parsed = Fdt::from_bytes(&encoded).unwrap();
    let soc = parsed.get_by_path("/soc").unwrap();
    assert_eq!(soc.name(), "soc");

    let uart = parsed.get_by_path("/soc/uart@1000").unwrap();
    assert_eq!(uart.name(), "uart@1000");
}

/// 测试 Round-trip: 解析 -> 编码 -> 解析
#[test]
fn test_parse_and_encode() {
    // 使用 Phytium DTB 进行测试
    let raw_data = fdt_phytium();
    let original = Fdt::from_bytes(&raw_data).unwrap();

    // 编码
    let encoded = original.encode();

    // 再次解析
    let reparsed = Fdt::from_bytes(&encoded).unwrap();

    // 验证 boot_cpuid_phys 一致
    assert_eq!(original.boot_cpuid_phys, reparsed.boot_cpuid_phys);

    // 验证节点数量一致
    assert_eq!(original.node_count(), reparsed.node_count());

    // 验证内存保留区一致
    assert_eq!(
        original.memory_reservations.len(),
        reparsed.memory_reservations.len()
    );
    for (orig, rep) in original
        .memory_reservations
        .iter()
        .zip(reparsed.memory_reservations.iter())
    {
        assert_eq!(orig.address, rep.address);
        assert_eq!(orig.size, rep.size);
    }

    // 验证所有节点路径都能找到
    for id in original.iter_node_ids() {
        let path = original.path_of(id);
        let reparsed_id = reparsed.get_by_path_id(&path);
        assert!(
            reparsed_id.is_some(),
            "path {} not found in reparsed FDT",
            path
        );
    }
}

/// 测试使用 Raspberry Pi 4 DTB 的 Round-trip
#[test]
fn test_parse_and_encode_rpi() {
    let raw_data = fdt_rpi_4b();
    let original = Fdt::from_bytes(&raw_data).unwrap();

    let encoded = original.encode();
    let reparsed = Fdt::from_bytes(&encoded).unwrap();

    assert_eq!(original.boot_cpuid_phys, reparsed.boot_cpuid_phys);
    assert_eq!(original.node_count(), reparsed.node_count());
}

/// 测试带内存保留区的 FDT 编码
#[test]
fn test_encode_with_memory_reservations() {
    let mut fdt = Fdt::new();

    // 添加内存保留区
    fdt.memory_reservations.push(fdt_raw::MemoryReservation {
        address: 0x8000_0000,
        size: 0x1000,
    });
    fdt.memory_reservations.push(fdt_raw::MemoryReservation {
        address: 0x9000_0000,
        size: 0x2000,
    });

    let encoded = fdt.encode();
    let reparsed = Fdt::from_bytes(&encoded).unwrap();

    // 验证内存保留区
    assert_eq!(reparsed.memory_reservations.len(), 2);
    assert_eq!(reparsed.memory_reservations[0].address, 0x8000_0000);
    assert_eq!(reparsed.memory_reservations[0].size, 0x1000);
    assert_eq!(reparsed.memory_reservations[1].address, 0x9000_0000);
    assert_eq!(reparsed.memory_reservations[1].size, 0x2000);
}

/// 测试使用真实带保留区的 DTB
#[test]
fn test_encode_with_reserve_dtb() {
    let raw_data = fdt_reserve();
    let original = Fdt::from_bytes(&raw_data).unwrap();

    let encoded = original.encode();
    let reparsed = Fdt::from_bytes(&encoded).unwrap();

    // 验证保留区被正确编码
    assert_eq!(
        original.memory_reservations.len(),
        reparsed.memory_reservations.len()
    );
}

/// 测试节点属性完整性
#[test]
fn test_encode_properties_integrity() {
    let mut fdt = Fdt::new();

    // 添加各种类型的属性
    let root_id = fdt.root_id();
    let node = fdt.node_mut(root_id).unwrap();

    // u32 属性
    node.set_property(crate::Property::new(
        "prop-u32",
        0x12345678u32.to_be_bytes().to_vec(),
    ));

    // u64 属性
    node.set_property(crate::Property::new(
        "prop-u64",
        0x1234567890ABCDEFu64.to_be_bytes().to_vec(),
    ));

    // 字符串属性
    node.set_property(crate::Property::new("prop-string", {
        let mut v = b"test string".to_vec();
        v.push(0);
        v
    }));

    // 字符串列表
    {
        let v = b"first\0second\0third\0".to_vec();
        node.set_property(crate::Property::new("prop-string-list", v));
    }

    // reg 属性
    {
        let v = vec![
            0x00, 0x10, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x40,
            0x00, 0x00,
        ];
        node.set_property(crate::Property::new("reg", v));
    }

    let encoded = fdt.encode();
    let reparsed = Fdt::from_bytes(&encoded).unwrap();

    // 验证各种类型的属性
    let root = reparsed.get_by_path("/").unwrap();
    let node_ref = root.as_node();

    // u32
    let prop_u32 = node_ref.get_property("prop-u32").unwrap();
    assert_eq!(prop_u32.get_u32(), Some(0x12345678));

    // u64
    let prop_u64 = node_ref.get_property("prop-u64").unwrap();
    assert_eq!(prop_u64.get_u64(), Some(0x1234567890ABCDEF));

    // string
    let prop_string = node_ref.get_property("prop-string").unwrap();
    assert_eq!(prop_string.as_str(), Some("test string"));

    // string list
    let prop_list = node_ref.get_property("prop-string-list").unwrap();
    let strings: Vec<&str> = prop_list.as_str_iter().collect();
    assert_eq!(strings, vec!["first", "second", "third"]);
}
