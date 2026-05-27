use aarch64_sysreg::RegistersType;

#[test]
fn test_registers_type_values() {
    assert_eq!(RegistersType::NONE as usize, 0x0);
    assert_eq!(RegistersType::W0 as usize, 0x1);
    assert_eq!(RegistersType::X0 as usize, 0x22);
    assert_eq!(RegistersType::SP as usize, 0x42);
    assert_eq!(RegistersType::V0 as usize, 0x43);
    assert_eq!(RegistersType::Q0 as usize, 0xe3);
}

#[test]
fn test_registers_type_display() {
    assert_eq!(format!("{}", RegistersType::NONE), "NONE");
    assert_eq!(format!("{}", RegistersType::W0), "W0");
    assert_eq!(format!("{}", RegistersType::X0), "X0");
    assert_eq!(format!("{}", RegistersType::XZR), "XZR");
    assert_eq!(format!("{}", RegistersType::WZR), "WZR");
    assert_eq!(format!("{}", RegistersType::SP), "SP");
    assert_eq!(format!("{}", RegistersType::WSP), "WSP");
}

#[test]
fn test_registers_type_lower_hex() {
    assert_eq!(format!("{:x}", RegistersType::W0), "1");
    assert_eq!(format!("{:x}", RegistersType::X0), "22");
    assert_eq!(format!("{:x}", RegistersType::SP), "42");
}

#[test]
fn test_registers_type_upper_hex() {
    assert_eq!(format!("{:X}", RegistersType::W0), "1");
    assert_eq!(format!("{:X}", RegistersType::X0), "22");
    assert_eq!(format!("{:X}", RegistersType::Q0), "E3");
}

#[test]
fn test_registers_type_from_usize() {
    assert_eq!(RegistersType::from(0x0), RegistersType::NONE);
    assert_eq!(RegistersType::from(0x1), RegistersType::W0);
    assert_eq!(RegistersType::from(0x22), RegistersType::X0);
    assert_eq!(RegistersType::from(0x42), RegistersType::SP);
}

#[test]
#[should_panic(expected = "Invalid register value")]
fn test_registers_type_from_invalid() {
    let _ = RegistersType::from(0xFFFF);
}

#[test]
fn test_registers_type_clone_copy() {
    let reg = RegistersType::X0;
    let reg_clone = reg.clone();
    let reg_copy = reg;
    assert_eq!(reg, reg_clone);
    assert_eq!(reg, reg_copy);
}

#[test]
fn test_registers_type_partial_eq() {
    assert_eq!(RegistersType::X0, RegistersType::X0);
    assert_ne!(RegistersType::X0, RegistersType::X1);
    assert_ne!(RegistersType::W0, RegistersType::X0);
}
