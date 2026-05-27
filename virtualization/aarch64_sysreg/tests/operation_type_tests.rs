use aarch64_sysreg::OperationType;

#[test]
fn test_operation_type_values() {
    assert_eq!(OperationType::ERROR as usize, 0x0);
    assert_eq!(OperationType::ABS as usize, 0x1);
    assert_eq!(OperationType::ADD as usize, 0x6);
    assert_eq!(OperationType::SUB as usize, 0x3d8);
    assert_eq!(OperationType::ZIP2 as usize, 0x49f);
}

#[test]
fn test_operation_type_display() {
    assert_eq!(format!("{}", OperationType::ERROR), "ERROR");
    assert_eq!(format!("{}", OperationType::ADD), "ADD");
    assert_eq!(format!("{}", OperationType::SUB), "SUB");
    assert_eq!(format!("{}", OperationType::MUL), "MUL");
    assert_eq!(format!("{}", OperationType::RET), "RET");
    assert_eq!(format!("{}", OperationType::BL), "BL");
}

#[test]
fn test_operation_type_lower_hex() {
    assert_eq!(format!("{:x}", OperationType::ADD), "6");
    assert_eq!(format!("{:x}", OperationType::SUB), "3d8");
    assert_eq!(format!("{:x}", OperationType::ERROR), "0");
}

#[test]
fn test_operation_type_upper_hex() {
    assert_eq!(format!("{:X}", OperationType::ADD), "6");
    assert_eq!(format!("{:X}", OperationType::SUB), "3D8");
    assert_eq!(format!("{:X}", OperationType::ERROR), "0");
}

#[test]
fn test_operation_type_from_usize() {
    assert_eq!(OperationType::from(0x0), OperationType::ERROR);
    assert_eq!(OperationType::from(0x1), OperationType::ABS);
    assert_eq!(OperationType::from(0x6), OperationType::ADD);
    assert_eq!(OperationType::from(0x3d8), OperationType::SUB);
}

#[test]
#[should_panic(expected = "Invalid arm64 operation value")]
fn test_operation_type_from_invalid() {
    let _ = OperationType::from(0xFFFF);
}

#[test]
fn test_operation_type_clone_copy() {
    let op = OperationType::ADD;
    let op_clone = op.clone();
    let op_copy = op;
    assert_eq!(op, op_clone);
    assert_eq!(op, op_copy);
}

#[test]
fn test_operation_type_partial_eq() {
    assert_eq!(OperationType::ADD, OperationType::ADD);
    assert_ne!(OperationType::ADD, OperationType::SUB);
}
