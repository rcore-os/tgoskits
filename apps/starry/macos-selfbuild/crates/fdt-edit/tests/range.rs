use dtb_file::*;
use fdt_edit::*;

#[test]
fn test_reg_address_translation() {
    let raw = fdt_rpi_4b();
    let fdt = Fdt::from_bytes(&raw).unwrap();

    // 测试 /soc/serial@7e215040 节点
    // bus address: 0x7e215040, CPU address: 0xfe215040
    let node = fdt.get_by_path("/soc/serial@7e215040").unwrap();
    let regs = node.regs();

    assert!(!regs.is_empty(), "should have at least one reg entry");

    let reg = &regs[0];
    assert_eq!(reg.address, 0xfe215040, "CPU address should be 0xfe215040");
    assert_eq!(
        reg.child_bus_address, 0x7e215040,
        "bus address should be 0x7e215040"
    );
    assert_eq!(reg.size, Some(0x40), "size should be 0x40");
}

#[test]
fn test_set_regs_with_ranges_conversion() {
    let raw = fdt_rpi_4b();
    let mut fdt = Fdt::from_bytes(&raw).unwrap();

    // 使用 CPU 地址设置 reg
    let new_cpu_address = 0xfe215080u64;
    let new_size = 0x80u64;
    {
        let mut node = fdt.get_by_path_mut("/soc/serial@7e215040").unwrap();
        node.set_regs(&[RegInfo {
            address: new_cpu_address,
            size: Some(new_size),
        }]);
    }

    // 重新读取验证
    let node = fdt.get_by_path("/soc/serial@7e215040").unwrap();
    let updated_regs = node.regs();
    let updated_reg = &updated_regs[0];

    // 验证：读取回来的 CPU 地址应该是我们设置的值
    assert_eq!(updated_reg.address, new_cpu_address);
    // 验证：bus 地址应该是转换后的值
    assert_eq!(updated_reg.child_bus_address, 0x7e215080);
    assert_eq!(updated_reg.size, Some(new_size));
}

#[test]
fn test_set_regs_roundtrip() {
    let raw = fdt_rpi_4b();
    let mut fdt = Fdt::from_bytes(&raw).unwrap();

    // 获取原始 reg 信息
    let original_reg = {
        let node = fdt.get_by_path("/soc/serial@7e215040").unwrap();
        node.regs()[0]
    };

    // 使用相同的 CPU 地址重新设置 reg
    {
        let mut node = fdt.get_by_path_mut("/soc/serial@7e215040").unwrap();
        node.set_regs(&[RegInfo {
            address: original_reg.address,
            size: original_reg.size,
        }]);
    }

    // 验证 roundtrip
    let roundtrip_reg = {
        let node = fdt.get_by_path("/soc/serial@7e215040").unwrap();
        node.regs()[0]
    };

    assert_eq!(roundtrip_reg.address, original_reg.address);
    assert_eq!(
        roundtrip_reg.child_bus_address,
        original_reg.child_bus_address
    );
    assert_eq!(roundtrip_reg.size, original_reg.size);
}
