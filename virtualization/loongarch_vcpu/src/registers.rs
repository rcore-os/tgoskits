use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use ax_cpu_local::CpuPin;
use tock_registers::{LocalRegisterCopy, register_bitfields};

pub const CSR_GSTAT: u16 = 0x50;
pub const CSR_CRMD: u16 = 0x0;
pub const CSR_PRMD: u16 = 0x1;
pub const CSR_EENTRY: u16 = 0x0c;
pub const CSR_ASID: u16 = 0x18;
pub const CSR_GCTL: u16 = 0x51;
pub const CSR_GTLBC: u16 = 0x15;
pub const CSR_GINTC: u16 = 0x52;
pub const CSR_PGDL: u16 = 0x19;
pub const CSR_PGDH: u16 = 0x1a;
pub const CSR_PWCL: u16 = 0x1c;
pub const CSR_PWCH: u16 = 0x1d;
pub const CSR_STLBPS: u16 = 0x1e;
pub const CSR_TLBRENTRY: u16 = 0x88;
pub const CSR_ECFG: u16 = 0x4;
pub const CSR_KSAVE_KSP: u16 = 0x30;

pub const GCSR_ESTAT: usize = 0x5;
pub const GCSR_EENTRY: usize = 0x0c;

pub const GINTC_HWIS_MASK: usize = 0xff;
pub const GINTC_HWIS_SHIFT: usize = 0;
pub const GINTC_HWIP_MASK: usize = 0xff << 8;
pub const GINTC_HWIP_SHIFT: usize = 8;

pub const INT_HWI0: usize = 2;
pub const INT_HWI7: usize = 9;
pub const INT_TIMER: usize = 11;
pub const INT_IPI: usize = 12;

static INJECT_INT_LOCK: InjectInterruptLock = InjectInterruptLock::new();
static INJECT_INT_LOGS: AtomicUsize = AtomicUsize::new(0);

struct InjectInterruptLock(AtomicBool);

impl InjectInterruptLock {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn lock(&self) -> InjectInterruptGuard<'_> {
        while self
            .0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            while self.0.load(Ordering::Acquire) {
                spin_loop();
            }
        }
        InjectInterruptGuard(self)
    }
}

struct InjectInterruptGuard<'a>(&'a InjectInterruptLock);

impl Drop for InjectInterruptGuard<'_> {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

register_bitfields! [
    usize,

    pub GSTAT [
        PGM OFFSET(2) NUMBITS(1) [],
        GID OFFSET(16) NUMBITS(8) []
    ],

    pub GTLBC [
        USE_TGID OFFSET(12) NUMBITS(1) [],
        TGID OFFSET(16) NUMBITS(8) []
    ],

    pub GCTL [
        MATC OFFSET(4) NUMBITS(2) [],
        TOPI OFFSET(7) NUMBITS(1) [],
        TOTI OFFSET(9) NUMBITS(1) [],
        TOE OFFSET(11) NUMBITS(1) [],
        TOP OFFSET(13) NUMBITS(1) [],
        TOHU OFFSET(15) NUMBITS(1) [],
        TOCI OFFSET(20) NUMBITS(2) [],
        GPM_NUM OFFSET(24) NUMBITS(3) []
    ],

    pub GINTC [
        HWIS OFFSET(0) NUMBITS(8) [],
        HWIP OFFSET(8) NUMBITS(8) []
    ],

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

    pub TLBRERA [
        ISTLBR OFFSET(0) NUMBITS(1) []
    ],

    pub TLBEHI [
        VPPN OFFSET(13) NUMBITS(35) []
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

pub const fn gintc_hwis_value(mask: usize) -> usize {
    GINTC::HWIS.val(mask).value
}

pub const fn gintc_hwip_value(mask: usize) -> usize {
    GINTC::HWIP.val(mask).value
}

pub const fn ecfg_line_mask(line: usize) -> usize {
    1usize << line
}

pub const fn ecfg_vs_value(vs: usize) -> usize {
    ECFG::VS.val(vs).value
}

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

pub const fn guest_tcfg_value(enabled: bool, periodic: bool, initval: usize) -> usize {
    let en = if enabled { TCFG::EN::SET.value } else { 0 };
    let periodic = if periodic {
        TCFG::PERIODIC::SET.value
    } else {
        0
    };
    en | periodic | TCFG::INITVAL.val(initval >> 2).value
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

#[inline(always)]
pub(crate) unsafe fn csr_read<const CSR_NUM: u16>() -> usize {
    let value: usize;
    core::arch::asm!("csrrd {}, {}", out(reg) value, const CSR_NUM);
    value
}

#[inline(always)]
pub(crate) unsafe fn csr_write<const CSR_NUM: u16>(value: usize) {
    core::arch::asm!("csrwr {}, {}", inout(reg) value => _, const CSR_NUM);
}

#[inline(always)]
pub(crate) unsafe fn gcsr_read<const GCSR_NUM: usize>() -> usize {
    let value: usize;
    core::arch::asm!("gcsrrd {}, {}", out(reg) value, const GCSR_NUM);
    value
}

#[inline(always)]
pub(crate) unsafe fn gcsr_write<const GCSR_NUM: usize>(value: usize) {
    core::arch::asm!("gcsrwr {}, {}", inout(reg) value => _, const GCSR_NUM);
}

#[inline(always)]
pub(crate) fn gstat_read() -> usize {
    unsafe { csr_read::<CSR_GSTAT>() }
}

#[inline(always)]
pub(crate) unsafe fn gstat_write(value: usize) {
    csr_write::<CSR_GSTAT>(value);
}

#[inline(always)]
pub(crate) unsafe fn gstat_set_gid(gid: usize) {
    let current = csr_read::<CSR_GSTAT>();
    let field = GSTAT::GID.val(gid);
    csr_write::<CSR_GSTAT>((current & !field.mask()) | field.value);
}

#[inline(always)]
pub(crate) unsafe fn gstat_set_pgm(pgm: bool) {
    let current = csr_read::<CSR_GSTAT>();
    let field = if pgm { GSTAT::PGM::SET.value } else { 0 };
    csr_write::<CSR_GSTAT>((current & !GSTAT::PGM::SET.mask()) | field);
}

#[inline(always)]
pub(crate) unsafe fn gtlbc_set_use_tgid(use_tgid: bool) {
    let current = csr_read::<CSR_GTLBC>();
    let field = if use_tgid {
        GTLBC::USE_TGID::SET.value
    } else {
        0
    };
    csr_write::<CSR_GTLBC>((current & !GTLBC::USE_TGID::SET.mask()) | field);
}

#[inline(always)]
pub(crate) unsafe fn gtlbc_set_tgid(tgid: usize) {
    let current = csr_read::<CSR_GTLBC>();
    let field = GTLBC::TGID.val(tgid);
    csr_write::<CSR_GTLBC>((current & !field.mask()) | field.value);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_matc(matc: usize) {
    let field = GCTL::MATC.val(matc);
    update_gctl(field.mask(), field.value);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_topi(topi: bool) {
    update_gctl_bool(GCTL::TOPI::SET.mask(), GCTL::TOPI::SET.value, topi);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toti(toti: bool) {
    update_gctl_bool(GCTL::TOTI::SET.mask(), GCTL::TOTI::SET.value, toti);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toe(toe: bool) {
    update_gctl_bool(GCTL::TOE::SET.mask(), GCTL::TOE::SET.value, toe);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_top(top: bool) {
    update_gctl_bool(GCTL::TOP::SET.mask(), GCTL::TOP::SET.value, top);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_tohu(tohu: bool) {
    update_gctl_bool(GCTL::TOHU::SET.mask(), GCTL::TOHU::SET.value, tohu);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toci(toci: usize) {
    let field = GCTL::TOCI.val(toci);
    update_gctl(field.mask(), field.value);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_gpm_num(gpm_num: usize) {
    let field = GCTL::GPM_NUM.val(gpm_num);
    update_gctl(field.mask(), field.value);
}

#[inline(always)]
unsafe fn update_gctl(mask: usize, value: usize) {
    let current = csr_read::<CSR_GCTL>();
    csr_write::<CSR_GCTL>((current & !mask) | value);
}

#[inline(always)]
unsafe fn update_gctl_bool(mask: usize, field: usize, enabled: bool) {
    update_gctl(mask, if enabled { field } else { 0 });
}

#[inline(always)]
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC::HWIP.val(usize::MAX).mask();
    // Do NOT clear HWIC here: writing HWIC bits clears the corresponding
    // HWIS (pending) bits in GINTC.  If an HWI was pending for the guest
    // while a timer-induced VM exit was being handled, setting HWIC would
    // silently discard that interrupt, eventually starving the guest's
    // UART driver and freezing the console.
    gintc |= gintc_hwip_value(mask);
    write_gintc(gintc);
}

#[inline(always)]
pub(crate) unsafe fn set_prmd_pie(pie: bool) {
    let current = csr_read::<CSR_PRMD>();
    let field = if pie { PRMD::PIE::SET.value } else { 0 };
    csr_write::<CSR_PRMD>((current & !PRMD::PIE::SET.mask()) | field);
}

#[inline(always)]
pub(crate) fn gcsr_eentry_read() -> usize {
    unsafe { gcsr_read::<GCSR_EENTRY>() }
}

pub(crate) fn read_gintc() -> usize {
    unsafe { csr_read::<CSR_GINTC>() }
}

pub(crate) unsafe fn write_gintc(value: usize) {
    csr_write::<CSR_GINTC>(value);
}

fn current_hwis() -> usize {
    LocalRegisterCopy::<usize, GINTC::Register>::new(read_gintc()).read(GINTC::HWIS)
}

pub(crate) fn set_hwi_interrupts(_cpu_pin: &CpuPin, mask: usize) {
    let hwis = (mask >> INT_HWI0) & GINTC_HWIS_MASK;
    unsafe {
        let mut gintc = read_gintc();
        gintc &= !GINTC::HWIS.val(usize::MAX).mask();
        gintc |= gintc_hwis_value(hwis);
        write_gintc(gintc);
    }
}

unsafe fn pulse_hwi(vector: usize) {
    let hwis_bit = 1 << (vector - INT_HWI0);

    let mut gintc = read_gintc();
    let cleared_hwis = current_hwis() & !hwis_bit;
    gintc &= !GINTC::HWIS.val(usize::MAX).mask();
    gintc |= gintc_hwis_value(cleared_hwis);
    write_gintc(gintc);

    let mut gintc = read_gintc();
    gintc &= !GINTC::HWIS.val(usize::MAX).mask();
    gintc |= gintc_hwis_value(cleared_hwis | hwis_bit);
    write_gintc(gintc);
}

pub fn inject_interrupt(_cpu_pin: &CpuPin, vector: usize) {
    if vector > INT_IPI {
        log::warn!("LoongArch64: invalid interrupt vector {vector}");
        return;
    }

    let _guard = INJECT_INT_LOCK.lock();

    unsafe {
        if (INT_HWI0..=INT_HWI7).contains(&vector) {
            let log_index = INJECT_INT_LOGS.fetch_add(1, Ordering::Relaxed);
            if log_index < 16 {
                log::trace!(
                    "LoongArch vcpu pulse HWI before: vector={}, gintc={:#x}, hwis={:#x}",
                    vector,
                    read_gintc(),
                    current_hwis()
                );
            }
            pulse_hwi(vector);
            if log_index < 16 {
                log::trace!(
                    "LoongArch vcpu pulse HWI after: vector={}, gintc={:#x}, hwis={:#x}",
                    vector,
                    read_gintc(),
                    current_hwis()
                );
            }
        } else {
            let estat = gcsr_read::<GCSR_ESTAT>();
            gcsr_write::<GCSR_ESTAT>(estat | (1usize << vector));
        }
    }
}
