use tock_registers::{LocalRegisterCopy, register_bitfields};

pub const INT_HWI0: usize = 2;
pub const INT_IPI: usize = 12;

register_bitfields! [
    usize,





    pub ECFG [
        LIE OFFSET(0) NUMBITS(13) [],
        VS OFFSET(16) NUMBITS(3) []
    ],

    pub CRMD [
        PLV OFFSET(0) NUMBITS(2) [],
        IE OFFSET(2) NUMBITS(1) [],
        DA OFFSET(3) NUMBITS(1) [],
        PG OFFSET(4) NUMBITS(1) []
    ],

    pub PRMD [
        PPLV OFFSET(0) NUMBITS(2) [],
        PIE OFFSET(2) NUMBITS(1) []
    ],

    pub ESTAT [
        IS OFFSET(0) NUMBITS(13) [],
        ECODE OFFSET(16) NUMBITS(6) [],
        ESUBCODE OFFSET(22) NUMBITS(9) []
    ],



    pub TCFG [
        EN OFFSET(0) NUMBITS(1) [],
        PERIODIC OFFSET(1) NUMBITS(1) [],
        INITVAL OFFSET(2) NUMBITS(usize::BITS as usize - 2) []
    ],

    pub TICLR [
        TI OFFSET(0) NUMBITS(1) []
    ],

    pub IOCSR_SEND [
        ACTION OFFSET(0) NUMBITS(5) [],
        CPU OFFSET(16) NUMBITS(10) [],
        BYTE_MASK OFFSET(27) NUMBITS(4) [],
        BUF OFFSET(32) NUMBITS(32) []
    ],

    pub IOCSR_MBUF_SEND [
        BOX OFFSET(2) NUMBITS(3) [],
        CPU OFFSET(16) NUMBITS(10) [],
        BUF OFFSET(32) NUMBITS(32) []
    ],

    pub EXTIOI_FEATURES [
        VIRT_EXTENSION OFFSET(0) NUMBITS(1) [],
        ENABLE_OPTION OFFSET(1) NUMBITS(1) [],
        INT_ENCODE OFFSET(2) NUMBITS(1) [],
        CPU_ENCODE OFFSET(3) NUMBITS(1) []
    ],

    pub EXTIOI_VIRT_CONFIG_REG [
        ENABLE_CPU_ENCODE OFFSET(3) NUMBITS(1) []
    ]
];

pub fn ecfg_vs_value_from(value: usize) -> usize {
    LocalRegisterCopy::<usize, ECFG::Register>::new(value).read(ECFG::VS)
}

pub fn estat_exception_mask() -> usize {
    ESTAT::ECODE.val(usize::MAX).mask() | ESTAT::ESUBCODE.val(usize::MAX).mask()
}

pub fn estat_exception_value(ecode: usize, esubcode: usize) -> usize {
    ESTAT::ECODE.val(ecode).value | ESTAT::ESUBCODE.val(esubcode).value
}

pub fn crmd_saved_state_mask() -> usize {
    CRMD::PLV.val(usize::MAX).mask() | CRMD::IE::SET.mask()
}

pub fn crmd_exception_clear_mask() -> usize {
    crmd_saved_state_mask()
}

pub fn crmd_saved_state(value: usize) -> usize {
    value & crmd_saved_state_mask()
}

pub const fn crmd_interrupt_enable_value() -> usize {
    CRMD::IE::SET.value
}

pub const fn crmd_direct_address_value() -> usize {
    CRMD::DA::SET.value
}

pub const fn crmd_paging_value() -> usize {
    CRMD::PG::SET.value
}

pub fn crmd_with_direct_addressing(value: usize) -> usize {
    (value | crmd_direct_address_value()) & !(crmd_paging_value() | crmd_exception_clear_mask())
}

pub fn prmd_saved_state_mask() -> usize {
    PRMD::PPLV.val(usize::MAX).mask() | PRMD::PIE::SET.mask()
}

pub const fn guest_tcfg_enable_mask() -> usize {
    TCFG::EN::SET.value
}

pub fn guest_tcfg_enabled(value: usize) -> bool {
    value & guest_tcfg_enable_mask() != 0
}

pub fn guest_tcfg_periodic(value: usize) -> bool {
    value & TCFG::PERIODIC::SET.value != 0
}

pub fn guest_tcfg_initval(value: usize) -> usize {
    LocalRegisterCopy::<usize, TCFG::Register>::new(value).read(TCFG::INITVAL) << 2
}

pub const fn guest_ticlr_clear_timer_value() -> usize {
    TICLR::TI::SET.value
}

pub fn guest_ticlr_has_timer_interrupt_clear(value: usize) -> bool {
    value & guest_ticlr_clear_timer_value() != 0
}

pub fn iocsr_send_action(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_SEND::Register>::new(value).read(IOCSR_SEND::ACTION)
}

pub fn iocsr_send_cpu(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_SEND::Register>::new(value).read(IOCSR_SEND::CPU)
}

pub fn iocsr_send_byte_mask(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_SEND::Register>::new(value).read(IOCSR_SEND::BYTE_MASK)
}

pub fn iocsr_send_data(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_SEND::Register>::new(value).read(IOCSR_SEND::BUF)
}

pub fn iocsr_mbuf_send_box(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_MBUF_SEND::Register>::new(value).read(IOCSR_MBUF_SEND::BOX)
}

pub fn iocsr_mbuf_send_cpu(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_MBUF_SEND::Register>::new(value).read(IOCSR_MBUF_SEND::CPU)
}

pub fn iocsr_mbuf_send_buf(value: usize) -> usize {
    LocalRegisterCopy::<usize, IOCSR_MBUF_SEND::Register>::new(value).read(IOCSR_MBUF_SEND::BUF)
}

pub const fn extioi_features_value() -> usize {
    EXTIOI_FEATURES::VIRT_EXTENSION::SET.value
        | EXTIOI_FEATURES::ENABLE_OPTION::SET.value
        | EXTIOI_FEATURES::INT_ENCODE::SET.value
        | EXTIOI_FEATURES::CPU_ENCODE::SET.value
}

pub fn extioi_cpu_encode_enabled(value: usize) -> bool {
    value & EXTIOI_VIRT_CONFIG_REG::ENABLE_CPU_ENCODE::SET.value != 0
}
