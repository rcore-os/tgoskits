use spin::Mutex;

pub const CSR_GSTAT: u16 = 0x50;
pub const CSR_EENTRY: u16 = 0x0c;
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

pub const GCSR_ESTAT: usize = 0x5;
pub const GCSR_CRMD: usize = 0x0;
pub const GCSR_PRMD: usize = 0x1;
pub const GCSR_EUEN: usize = 0x2;
pub const GCSR_MISC: usize = 0x3;
pub const GCSR_ECTL: usize = 0x4;
pub const GCSR_ERA: usize = 0x6;
pub const GCSR_BADV: usize = 0x7;
pub const GCSR_BADI: usize = 0x8;
pub const GCSR_EENTRY: usize = 0x0c;
pub const GCSR_TLBIDX: usize = 0x10;
pub const GCSR_TLBEHI: usize = 0x11;
pub const GCSR_TLBELO0: usize = 0x12;
pub const GCSR_TLBELO1: usize = 0x13;
pub const GCSR_ASID: usize = 0x18;
pub const GCSR_PGDL: usize = 0x19;
pub const GCSR_PGDH: usize = 0x1a;
pub const GCSR_PGD: usize = 0x1b;
pub const GCSR_PWCL: usize = 0x1c;
pub const GCSR_PWCH: usize = 0x1d;
pub const GCSR_STLBPS: usize = 0x1e;
pub const GCSR_RAVCFG: usize = 0x1f;
pub const GCSR_CPUID: usize = 0x20;
pub const GCSR_PRCFG1: usize = 0x21;
pub const GCSR_PRCFG2: usize = 0x22;
pub const GCSR_PRCFG3: usize = 0x23;
pub const GCSR_SAVE0: usize = 0x30;
pub const GCSR_SAVE1: usize = 0x31;
pub const GCSR_SAVE2: usize = 0x32;
pub const GCSR_SAVE3: usize = 0x33;
pub const GCSR_SAVE4: usize = 0x34;
pub const GCSR_SAVE5: usize = 0x35;
pub const GCSR_SAVE6: usize = 0x36;
pub const GCSR_SAVE7: usize = 0x37;
pub const GCSR_SAVE8: usize = 0x38;
pub const GCSR_SAVE9: usize = 0x39;
pub const GCSR_SAVE10: usize = 0x3a;
pub const GCSR_SAVE11: usize = 0x3b;
pub const GCSR_SAVE12: usize = 0x3c;
pub const GCSR_SAVE13: usize = 0x3d;
pub const GCSR_SAVE14: usize = 0x3e;
pub const GCSR_SAVE15: usize = 0x3f;
pub const GCSR_TID: usize = 0x40;
pub const GCSR_TCFG: usize = 0x41;
pub const GCSR_TVAL: usize = 0x42;
pub const GCSR_CNTC: usize = 0x43;
pub const GCSR_TICLR: usize = 0x44;
pub const GCSR_LLBCTL: usize = 0x60;
pub const GCSR_TLBRENTRY: usize = 0x88;
pub const GCSR_TLBRBADV: usize = 0x89;
pub const GCSR_TLBRERA: usize = 0x8a;
pub const GCSR_TLBRSAVE: usize = 0x8b;
pub const GCSR_TLBRELO0: usize = 0x8c;
pub const GCSR_TLBRELO1: usize = 0x8d;
pub const GCSR_TLBREHI: usize = 0x8e;
pub const GCSR_TLBRPRMD: usize = 0x8f;
pub const GCSR_DMW0: usize = 0x180;
pub const GCSR_DMW1: usize = 0x181;
pub const GCSR_DMW2: usize = 0x182;
pub const GCSR_DMW3: usize = 0x183;

pub const GINTC_HWIS_MASK: usize = 0xff;
pub const GINTC_HWIS_SHIFT: usize = 0;
pub const GINTC_HWIP_MASK: usize = 0xff << 8;
pub const GINTC_HWIP_SHIFT: usize = 8;

pub const INT_HWI0: usize = 2;
pub const INT_HWI7: usize = 9;
pub const INT_TIMER: usize = 11;
pub const INT_IPI: usize = 12;

static INJECT_INT_LOCK: Mutex<()> = Mutex::new(());

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
    core::arch::asm!("csrwr {}, {}", in(reg) value, const CSR_NUM);
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
    core::arch::asm!("gcsrwr {}, {}", in(reg) value, const GCSR_NUM);
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
pub(crate) unsafe fn gstat_set_pgm(pgm: bool) {
    set_csr_bit::<CSR_GSTAT>(1, pgm);
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
pub(crate) unsafe fn gintc_set_hwip(mask: usize) {
    let mut gintc = read_gintc();
    gintc &= !GINTC_HWIP_MASK;
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
pub(crate) fn gcsr_eentry_read() -> usize {
    unsafe { gcsr_read::<GCSR_EENTRY>() }
}

fn read_gintc() -> usize {
    unsafe { csr_read::<CSR_GINTC>() }
}

unsafe fn write_gintc(value: usize) {
    csr_write::<CSR_GINTC>(value);
}

pub fn inject_interrupt(vector: usize) {
    if vector > INT_IPI {
        log::warn!("LoongArch64: invalid interrupt vector {vector}");
        return;
    }

    let _guard = INJECT_INT_LOCK.lock();

    unsafe {
        if (INT_HWI0..=INT_HWI7).contains(&vector) {
            let hwis_bit = 1 << (vector - INT_HWI0);
            let current_hwis = (read_gintc() & GINTC_HWIS_MASK) >> GINTC_HWIS_SHIFT;
            let mut gintc = read_gintc();
            gintc &= !GINTC_HWIS_MASK;
            gintc |= ((current_hwis | hwis_bit) << GINTC_HWIS_SHIFT) & GINTC_HWIS_MASK;
            write_gintc(gintc);
        } else {
            let estat = gcsr_read::<GCSR_ESTAT>();
            gcsr_write::<GCSR_ESTAT>(estat | (1usize << vector));
        }
    }
}
