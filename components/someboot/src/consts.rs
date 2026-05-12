#[cfg(target_os = "none")]
include!(concat!(env!("OUT_DIR"), "/defines.rs"));

#[cfg(page_size_4k)]
pub const PAGE_SIZE: usize = 4096;

#[cfg(page_size_16k)]
pub const PAGE_SIZE: usize = 16384;
