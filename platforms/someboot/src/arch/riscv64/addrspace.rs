include!(concat!(env!("OUT_DIR"), "/defines.rs"));

#[cfg(any(uspace, hv))]
pub const PAGE_OFFSET: usize = 0xffff_ffc0_0000_0000;
#[cfg(not(any(uspace, hv)))]
pub const PAGE_OFFSET: usize = 0;

#[cfg(any(uspace, hv))]
pub const PERCPU_BASE: usize = 0xffff_ffe0_0000_0000;
#[cfg(not(any(uspace, hv)))]
pub const PERCPU_BASE: usize = PAGE_OFFSET;
