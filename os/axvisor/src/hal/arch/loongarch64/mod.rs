#![allow(unsafe_op_in_unsafe_fn)]

mod api;
pub mod cache;

use spin::Mutex;

const CSR_GSTAT: u16 = 0x50;
const CSR_GINTC: u16 = 0x52;

const GCSR_ESTAT: usize = 0x5;

const GSTAT_PGM: usize = 1 << 1;
const GSTAT_GIDBITS_MASK: usize = 0x3f << 4;
const GSTAT_GIDBITS_SHIFT: usize = 4;
const GSTAT_GID_MASK: usize = 0xff << 16;
const GSTAT_GID_SHIFT: usize = 16;

const GINTC_HWIS_MASK: usize = 0xff;
const GINTC_HWIS_SHIFT: usize = 0;

const INT_HWI0: usize = 2;
const INT_HWI7: usize = 9;
const INT_IPI: usize = 12;

static INJECT_INT_LOCK: Mutex<()> = Mutex::new(());

#[inline(always)]
unsafe fn csr_read<const CSR_NUM: u16>() -> usize {
    let value: usize;
    core::arch::asm!("csrrd {}, {}", out(reg) value, const CSR_NUM);
    value
}

#[inline(always)]
unsafe fn csr_write<const CSR_NUM: u16>(value: usize) {
    core::arch::asm!("csrwr {}, {}", in(reg) value, const CSR_NUM);
}

#[inline(always)]
unsafe fn gcsr_read<const GCSR_NUM: usize>() -> usize {
    let value: usize;
    core::arch::asm!("gcsrrd {}, {}", out(reg) value, const GCSR_NUM);
    value
}

#[inline(always)]
unsafe fn gcsr_write<const GCSR_NUM: usize>(value: usize) {
    core::arch::asm!("gcsrwr {}, {}", in(reg) value, const GCSR_NUM);
}

#[inline(always)]
fn read_gstat() -> usize {
    unsafe { csr_read::<CSR_GSTAT>() }
}

#[inline(always)]
fn read_gintc() -> usize {
    unsafe { csr_read::<CSR_GINTC>() }
}

#[inline(always)]
unsafe fn write_gintc(value: usize) {
    csr_write::<CSR_GINTC>(value);
}

#[inline(always)]
fn read_gcsr_estat() -> usize {
    unsafe { gcsr_read::<GCSR_ESTAT>() }
}

#[inline(always)]
unsafe fn write_gcsr_estat(value: usize) {
    gcsr_write::<GCSR_ESTAT>(value);
}

#[inline(always)]
unsafe fn cpucfg_read(reg: u32) -> u32 {
    let value: u32;
    core::arch::asm!("cpucfg {}, {}", out(reg) value, in(reg) reg);
    value
}

fn has_virtualization_support() -> bool {
    let cpucfg2 = unsafe { cpucfg_read(2) };
    (cpucfg2 & (1 << 10)) != 0
}

fn get_gidbits() -> usize {
    (read_gstat() & GSTAT_GIDBITS_MASK) >> GSTAT_GIDBITS_SHIFT
}

fn max_gid() -> usize {
    let gidbits = get_gidbits();
    if gidbits == 0 { 0 } else { (1 << gidbits) - 1 }
}

fn current_gid() -> usize {
    (read_gstat() & GSTAT_GID_MASK) >> GSTAT_GID_SHIFT
}

fn current_hwis() -> usize {
    (read_gintc() & GINTC_HWIS_MASK) >> GINTC_HWIS_SHIFT
}

pub fn hardware_check() {
    let gstat = read_gstat();
    info!("CSR.GSTAT = 0x{gstat:x}");

    if has_virtualization_support() {
        info!("LoongArch virtualization extensions supported (CPUCFG.2.LVZ[10]=1)");
    } else {
        warn!("LoongArch virtualization extensions not supported (CPUCFG.2.LVZ[10]=0)");
    }

    if (gstat & GSTAT_PGM) != 0 {
        info!("Guest mode currently enabled (PGM=1)");
    } else {
        info!("Guest mode currently disabled (PGM=0)");
    }

    let gidbits = get_gidbits();
    info!(
        "GIDBITS value: {gidbits} (supports {} GIDs)",
        1usize << gidbits
    );
    info!("Maximum GID value: {}", max_gid());
    info!("Current GID value: {}", current_gid());
}

pub fn inject_interrupt(vector: usize) {
    if vector > INT_IPI {
        warn!("LoongArch64: invalid interrupt vector {vector}");
        return;
    }

    let _guard = INJECT_INT_LOCK.lock();

    unsafe {
        if (INT_HWI0..=INT_HWI7).contains(&vector) {
            let hwis_bit = 1 << (vector - INT_HWI0);
            let mut gintc = read_gintc();
            gintc &= !GINTC_HWIS_MASK;
            gintc |= ((current_hwis() | hwis_bit) << GINTC_HWIS_SHIFT) & GINTC_HWIS_MASK;
            write_gintc(gintc);
        } else {
            write_gcsr_estat(read_gcsr_estat() | (1usize << vector));
        }
    }
}
