#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/defines.rs"));

pub const PABITS: usize = 48;

const TO_PHYS_MASK: usize = (1 << PABITS) - 1;

pub const fn to_phys(addr: usize) -> usize {
    addr & TO_PHYS_MASK
}

pub const fn to_cache(addr: usize) -> usize {
    to_phys(addr) | CACHE_BASE
}

pub const fn to_uncache(addr: usize) -> usize {
    to_phys(addr) | UNCACHE_BASE
}

pub const DMW_DA_BITS: usize = PABITS;
pub const CSR_DMW0_PLV0: usize = 1 << 0;
pub const CSR_DMW0_VSEG: usize = 0x8000;
pub const CSR_DMW0_BASE: usize = CSR_DMW0_VSEG << DMW_DA_BITS;
pub const CSR_DMW0_INIT: usize = CSR_DMW0_BASE | CSR_DMW0_PLV0;

pub const CSR_DMW1_PLV0: usize = 1 << 0;
pub const CSR_DMW1_MAT: usize = 1 << 4;
pub const CSR_DMW1_VSEG: usize = 0x9000;
pub const CSR_DMW1_BASE: usize = CSR_DMW1_VSEG << DMW_DA_BITS;
pub const CSR_DMW1_INIT: usize = CSR_DMW1_BASE | CSR_DMW1_PLV0 | CSR_DMW1_MAT;

pub const CSR_DMW2_PLV0: usize = 1 << 0;
pub const CSR_DMW2_MAT: usize = 2 << 4;
pub const CSR_DMW2_VSEG: usize = 0xa000;
pub const CSR_DMW2_BASE: usize = CSR_DMW2_VSEG << DMW_DA_BITS;
pub const CSR_DMW2_INIT: usize = CSR_DMW2_BASE | CSR_DMW2_PLV0 | CSR_DMW2_MAT;

pub const UNCACHE_BASE: usize = CSR_DMW0_BASE;
pub const CACHE_BASE: usize = CSR_DMW1_BASE;
pub const IO_BASE: usize = UNCACHE_BASE;
pub const PAGE_OFFSET: usize = CACHE_BASE;
