use ax_plat::mem::RawRange;

pub const PLATFORM_NAME: &str = "custom";

pub const RAM_BASE: usize = 0x8000_0000;
pub const RAM_SIZE: usize = 128 * 1024 * 1024;

pub const RESERVED_BASE: usize = RAM_BASE;
pub const RESERVED_SIZE: usize = 2 * 1024 * 1024;

pub const KERNEL_ASPACE_BASE: usize = 0xffff_0000_0000_0000;
pub const KERNEL_ASPACE_SIZE: usize = 0x0001_0000_0000_0000;

pub const PHYS_RAM_RANGES: &[RawRange] = &[(RAM_BASE, RAM_SIZE)];
pub const RESERVED_RAM_RANGES: &[RawRange] = &[(RESERVED_BASE, RESERVED_SIZE)];
pub const MMIO_RANGES: &[RawRange] = &[];
