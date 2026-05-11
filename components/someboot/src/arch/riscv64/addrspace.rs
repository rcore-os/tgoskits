include!(concat!(env!("OUT_DIR"), "/defines.rs"));

#[cfg(uspace)]
pub const PAGE_OFFSET: usize = 0xffff_ffc0_0000_0000;
#[cfg(not(uspace))]
pub const PAGE_OFFSET: usize = 0;

#[cfg(uspace)]
pub const PERCPU_BASE: usize = 0xffff_ffe0_0000_0000;
#[cfg(not(uspace))]
pub const PERCPU_BASE: usize = PAGE_OFFSET;
