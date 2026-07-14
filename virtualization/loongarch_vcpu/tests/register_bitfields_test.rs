#![cfg(target_arch = "loongarch64")]

use loongarch_vcpu::registers::{
    crmd_direct_address_value, crmd_exception_clear_mask, crmd_paging_value, crmd_saved_state,
    crmd_saved_state_mask, ecfg_line_mask, ecfg_vs_value, ecfg_vs_value_from, estat_exception_mask,
    estat_exception_value, extioi_cpu_encode_enabled, extioi_features_value, gintc_hwip_value,
    gintc_hwis_value, guest_tcfg_enable_mask, guest_tcfg_enabled, guest_tcfg_initval,
    guest_tcfg_periodic, guest_tcfg_value, guest_ticlr_clear_timer_value,
    guest_ticlr_has_timer_interrupt_clear, iocsr_mbuf_send_box, iocsr_mbuf_send_buf,
    iocsr_mbuf_send_cpu, iocsr_send_action, iocsr_send_byte_mask, iocsr_send_cpu, iocsr_send_data,
    prmd_saved_state_mask,
};

#[test]
fn gintc_hwi_fields_match_existing_encoding() {
    assert_eq!(gintc_hwis_value(0xab), 0xab);
    assert_eq!(gintc_hwip_value(0xcd), 0xcd00);
}

#[test]
fn ecfg_fields_match_existing_encoding() {
    assert_eq!(ecfg_line_mask(11), 1 << 11);
    assert_eq!(ecfg_vs_value(5), 5 << 16);
    assert_eq!(ecfg_vs_value_from(5 << 16), 5);
}

#[test]
fn guest_timer_fields_match_existing_encoding() {
    assert_eq!(guest_tcfg_value(true, true, 0x1234), 0x1234 | 0b11);
    assert_eq!(guest_tcfg_enable_mask(), 1);
    assert!(guest_tcfg_enabled(0x1235));
    assert!(!guest_tcfg_enabled(0x1234));
    assert!(guest_tcfg_periodic(0x1236));
    assert_eq!(guest_tcfg_initval(0x1237), 0x1234);
    assert_eq!(guest_ticlr_clear_timer_value(), 1);
    assert!(guest_ticlr_has_timer_interrupt_clear(1));
}

#[test]
fn guest_exception_fields_match_existing_encoding() {
    assert_eq!(crmd_saved_state_mask(), 0b111);
    assert_eq!(crmd_exception_clear_mask(), 0b111);
    assert_eq!(crmd_saved_state(0b1111), 0b111);
    assert_eq!(crmd_direct_address_value(), 1 << 3);
    assert_eq!(crmd_paging_value(), 1 << 4);
    assert_eq!(prmd_saved_state_mask(), 0b111);
    assert_eq!(estat_exception_mask(), (0x3f << 16) | (0x1ff << 22));
    assert_eq!(
        estat_exception_value(0x12, 0x101),
        (0x12 << 16) | (0x101 << 22)
    );
}

#[test]
fn iocsr_send_fields_match_existing_encoding() {
    let send = (0x155 << 16) | (0xb << 27) | (0x1122_3344usize << 32) | 0x1a;
    assert_eq!(iocsr_send_action(send), 0x1a);
    assert_eq!(iocsr_send_cpu(send), 0x155);
    assert_eq!(iocsr_send_byte_mask(send), 0xb);
    assert_eq!(iocsr_send_data(send), 0x1122_3344);

    let mbuf = (0x2aa << 16) | (0x5 << 2) | (0xaabb_ccddusize << 32);
    assert_eq!(iocsr_mbuf_send_box(mbuf), 0x5);
    assert_eq!(iocsr_mbuf_send_cpu(mbuf), 0x2aa);
    assert_eq!(iocsr_mbuf_send_buf(mbuf), 0xaabb_ccdd);

    assert_eq!(extioi_features_value(), 0b1111);
    assert!(extioi_cpu_encode_enabled(1 << 3));
}
