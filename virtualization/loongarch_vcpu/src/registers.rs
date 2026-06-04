use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::SpinNoIrq as Mutex;

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

static INJECT_INT_LOCK: Mutex<()> = Mutex::new(());
static INJECT_INT_LOGS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub(crate) unsafe fn csr_read<const CSR_NUM: u16>() -> usize {
    let value: usize;
    core::arch::asm!("csrrd {}, {}", out(reg) value, const CSR_NUM);
    value
}

#[cfg(not(target_arch = "loongarch64"))]
#[inline(always)]
pub(crate) unsafe fn csr_read<const CSR_NUM: u16>() -> usize {
    let _ = CSR_NUM;
    0
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub(crate) unsafe fn csr_write<const CSR_NUM: u16>(value: usize) {
    core::arch::asm!("csrwr {}, {}", inout(reg) value => _, const CSR_NUM);
}

#[cfg(not(target_arch = "loongarch64"))]
#[inline(always)]
pub(crate) unsafe fn csr_write<const CSR_NUM: u16>(value: usize) {
    let _ = (CSR_NUM, value);
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub(crate) unsafe fn gcsr_read<const GCSR_NUM: usize>() -> usize {
    let value: usize;
    core::arch::asm!("gcsrrd {}, {}", out(reg) value, const GCSR_NUM);
    value
}

#[cfg(not(target_arch = "loongarch64"))]
#[inline(always)]
pub(crate) unsafe fn gcsr_read<const GCSR_NUM: usize>() -> usize {
    let _ = GCSR_NUM;
    0
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub(crate) unsafe fn gcsr_write<const GCSR_NUM: usize>(value: usize) {
    core::arch::asm!("gcsrwr {}, {}", inout(reg) value => _, const GCSR_NUM);
}

#[cfg(not(target_arch = "loongarch64"))]
#[inline(always)]
pub(crate) unsafe fn gcsr_write<const GCSR_NUM: usize>(value: usize) {
    let _ = (GCSR_NUM, value);
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
pub(crate) unsafe fn set_csr_bits<const CSR_NUM: u16>(
    range_lsb: usize,
    range_width: usize,
    value: usize,
) {
    let mask = ((1usize << range_width) - 1) << range_lsb;
    let current = csr_read::<CSR_NUM>();
    let new_value = (current & !mask) | ((value << range_lsb) & mask);
    csr_write::<CSR_NUM>(new_value);
}

#[inline(always)]
pub(crate) unsafe fn set_csr_bit<const CSR_NUM: u16>(bit: usize, value: bool) {
    let current = csr_read::<CSR_NUM>();
    let mask = 1usize << bit;
    let new_value = if value {
        current | mask
    } else {
        current & !mask
    };
    csr_write::<CSR_NUM>(new_value);
}

#[inline(always)]
pub(crate) unsafe fn gstat_set_gid(gid: usize) {
    set_csr_bits::<CSR_GSTAT>(16, 8, gid);
}

#[inline(always)]
pub(crate) unsafe fn gstat_set_pvm(pvm: bool) {
    set_csr_bit::<CSR_GSTAT>(1, pvm);
}

#[inline(always)]
pub(crate) unsafe fn gtlbc_set_use_tgid(use_tgid: bool) {
    set_csr_bit::<CSR_GTLBC>(12, use_tgid);
}

#[inline(always)]
pub(crate) unsafe fn gtlbc_set_tgid(tgid: usize) {
    set_csr_bits::<CSR_GTLBC>(16, 8, tgid);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_matc(matc: usize) {
    set_csr_bits::<CSR_GCTL>(4, 2, matc);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_topi(topi: bool) {
    set_csr_bit::<CSR_GCTL>(7, topi);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toti(toti: bool) {
    set_csr_bit::<CSR_GCTL>(9, toti);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toe(toe: bool) {
    set_csr_bit::<CSR_GCTL>(11, toe);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_top(top: bool) {
    set_csr_bit::<CSR_GCTL>(13, top);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_tohu(tohu: bool) {
    set_csr_bit::<CSR_GCTL>(15, tohu);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_toci(toci: usize) {
    set_csr_bits::<CSR_GCTL>(20, 2, toci);
}

#[inline(always)]
pub(crate) unsafe fn gcfg_set_gpm_num(gpm_num: usize) {
    set_csr_bits::<CSR_GCTL>(24, 3, gpm_num);
}

#[inline(always)]
pub(crate) unsafe fn gintc_set_hwi_passthrough(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
    // Do NOT clear HWIC here: writing HWIC bits clears the corresponding
    // HWIS (pending) bits in GINTC.  If an HWI was pending for the guest
    // while a timer-induced VM exit was being handled, setting HWIC would
    // silently discard that interrupt, eventually starving the guest's
    // UART driver and freezing the console.
    gintc |= (mask << GINTC_HWIP_SHIFT) & GINTC_HWIP_MASK;
    write_gintc(gintc);
}

#[inline(always)]
pub(crate) unsafe fn set_ecfg_line_enabled(line: usize, enabled: bool) {
    let bit = 1usize << line;
    let current = csr_read::<CSR_ECFG>();
    let new_value = if enabled {
        current | bit
    } else {
        current & !bit
    };
    csr_write::<CSR_ECFG>(new_value);
}

#[inline(always)]
pub(crate) unsafe fn set_ecfg_vs(vs: usize) {
    set_csr_bits::<CSR_ECFG>(16, 3, vs);
}

#[inline(always)]
pub(crate) unsafe fn get_ecfg_vs() -> usize {
    (csr_read::<CSR_ECFG>() >> 16) & 0x7
}

#[inline(always)]
pub(crate) fn gcsr_eentry_read() -> usize {
    unsafe { gcsr_read::<GCSR_EENTRY>() }
}

fn read_gintc() -> usize {
    unsafe { csr_read::<CSR_GINTC>() }
}

unsafe fn write_gintc(value: usize) {
    csr_write::<CSR_GINTC>(value);
}

fn current_hwis() -> usize {
    (read_gintc() & GINTC_HWIS_MASK) >> GINTC_HWIS_SHIFT
}

unsafe fn pulse_hwi(vector: usize) {
    let hwis_bit = 1 << (vector - INT_HWI0);

    let mut gintc = read_gintc();
    let cleared_hwis = current_hwis() & !hwis_bit;
    gintc &= !GINTC_HWIS_MASK;
    gintc |= (cleared_hwis << GINTC_HWIS_SHIFT) & GINTC_HWIS_MASK;
    write_gintc(gintc);

    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIS_MASK;
    gintc |= ((cleared_hwis | hwis_bit) << GINTC_HWIS_SHIFT) & GINTC_HWIS_MASK;
    write_gintc(gintc);
}

pub fn inject_interrupt(vector: usize) {
    if vector > INT_IPI {
        log::warn!("LoongArch64: invalid interrupt vector {vector}");
        return;
    }

    let _guard = INJECT_INT_LOCK.lock();

    unsafe {
        if (INT_HWI0..=INT_HWI7).contains(&vector) {
            if INJECT_INT_LOGS.fetch_add(1, Ordering::Relaxed) < 16 {
                log::debug!(
                    "LoongArch vcpu pulse HWI: vector={}, gintc_before={:#x}, hwis_before={:#x}",
                    vector,
                    read_gintc(),
                    current_hwis()
                );
            }
            pulse_hwi(vector);
        } else {
            let estat = gcsr_read::<GCSR_ESTAT>();
            gcsr_write::<GCSR_ESTAT>(estat | (1usize << vector));
        }
    }
}
