use aarch64_sysreg::SystemRegType;

#[test]
fn test_system_reg_type_values() {
    assert_eq!(SystemRegType::OSDTRRX_EL1 as usize, 0x240000);
    assert_eq!(SystemRegType::DBGBVR0_EL1 as usize, 0x280000);
    assert_eq!(SystemRegType::MDSCR_EL1 as usize, 0x240004);
}

#[test]
fn test_system_reg_type_display() {
    assert_eq!(format!("{}", SystemRegType::OSDTRRX_EL1), "OSDTRRX_EL1");
    assert_eq!(format!("{}", SystemRegType::DBGBVR0_EL1), "DBGBVR0_EL1");
    assert_eq!(format!("{}", SystemRegType::MDSCR_EL1), "MDSCR_EL1");
    assert_eq!(format!("{}", SystemRegType::PSTATE_SPSEL), "PSTATE_SPSEL");
}

#[test]
fn test_system_reg_type_lower_hex() {
    assert_eq!(format!("{:x}", SystemRegType::OSDTRRX_EL1), "240000");
    assert_eq!(format!("{:x}", SystemRegType::DBGBVR0_EL1), "280000");
}

#[test]
fn test_system_reg_type_upper_hex() {
    assert_eq!(format!("{:X}", SystemRegType::OSDTRRX_EL1), "240000");
    assert_eq!(format!("{:X}", SystemRegType::DBGBVR0_EL1), "280000");
}

#[test]
fn test_system_reg_type_from_usize() {
    assert_eq!(SystemRegType::from(0x240000), SystemRegType::OSDTRRX_EL1);
    assert_eq!(SystemRegType::from(0x280000), SystemRegType::DBGBVR0_EL1);
    assert_eq!(SystemRegType::from(0x240004), SystemRegType::MDSCR_EL1);
}

#[test]
#[should_panic(expected = "Invalid system register value")]
fn test_system_reg_type_from_invalid() {
    let _ = SystemRegType::from(0xFFFFFF);
}

#[test]
fn test_system_reg_type_clone_copy() {
    let reg = SystemRegType::MDSCR_EL1;
    let reg_clone = reg.clone();
    let reg_copy = reg;
    assert_eq!(reg, reg_clone);
    assert_eq!(reg, reg_copy);
}

#[test]
fn test_system_reg_type_partial_eq() {
    assert_eq!(SystemRegType::MDSCR_EL1, SystemRegType::MDSCR_EL1);
    assert_ne!(SystemRegType::MDSCR_EL1, SystemRegType::OSDTRRX_EL1);
}
