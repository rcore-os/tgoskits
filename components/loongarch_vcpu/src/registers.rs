use spin::Mutex;

pub const CSR_GSTAT: u16 = 0x50;
pub const CSR_EENTRY: u16 = 0x0c;
pub const CSR_GCTL: u16 = 0x51;
pub const CSR_GINTC: u16 = 0x52;

pub const GCSR_ESTAT: usize = 0x5;
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
pub const GCSR_TCFG: usize = 0x41;
pub const GCSR_TVAL: usize = 0x42;
pub const GCSR_TICLR: usize = 0x44;

pub const GINTC_HWIS_MASK: usize = 0xff;
pub const GINTC_HWIS_SHIFT: usize = 0;

pub const INT_HWI0: usize = 2;
pub const INT_HWI7: usize = 9;
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
pub(crate) fn gcsr_eentry_read() -> usize {
    unsafe { gcsr_read::<GCSR_EENTRY>() }
}

#[inline(always)]
pub(crate) unsafe fn gcsr_eentry_write(value: usize) {
    gcsr_write::<GCSR_EENTRY>(value);
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
