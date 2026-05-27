use riscv_vplic::*;

#[test]
fn test_plic_num_sources() {
    assert_eq!(PLIC_NUM_SOURCES, 1024);
}

#[test]
fn test_plic_priority_offset() {
    assert_eq!(PLIC_PRIORITY_OFFSET, 0x000000);
}

#[test]
fn test_plic_pending_offset() {
    assert_eq!(PLIC_PENDING_OFFSET, 0x001000);
}

#[test]
fn test_plic_enable_offset() {
    assert_eq!(PLIC_ENABLE_OFFSET, 0x002000);
}

#[test]
fn test_plic_enable_stride() {
    assert_eq!(PLIC_ENABLE_STRIDE, 0x80);
}

#[test]
fn test_plic_context_ctrl_offset() {
    assert_eq!(PLIC_CONTEXT_CTRL_OFFSET, 0x200000);
}

#[test]
fn test_plic_context_stride() {
    assert_eq!(PLIC_CONTEXT_STRIDE, 0x1000);
}

#[test]
fn test_plic_context_threshold_offset() {
    assert_eq!(PLIC_CONTEXT_THRESHOLD_OFFSET, 0x00);
}

#[test]
fn test_plic_context_claim_complete_offset() {
    assert_eq!(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, 0x04);
}
