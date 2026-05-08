//! Raw offsets and constants that mirror the hardware documentation for the
//! RKNN register file.
#![allow(dead_code)]

pub const PC_BASE_OFFSET: usize = 0x0000;
pub const INT_BASE_OFFSET: usize = 0x0020;
pub const CNA_BASE_OFFSET: usize = 0x1000;
pub const CORE_BASE_OFFSET: usize = 0x3000;
pub const DPU_BASE_OFFSET: usize = 0x4000;
pub const DPU_RDMA_BASE_OFFSET: usize = 0x5000;
pub const PPU_BASE_OFFSET: usize = 0x6000;
pub const PPU_RDMA_BASE_OFFSET: usize = 0x7000;
pub const DDMA_BASE_OFFSET: usize = 0x8000;
pub const SDMA_BASE_OFFSET: usize = 0x9000;
pub const GLOBAL_BASE_OFFSET: usize = 0xF000;

/// Offset of the global enable mask register (relative to GLOBAL base).
pub const OFFSET_ENABLE_MASK: usize = 0x0008;

/// Value written to acknowledge all interrupt sources.
pub const INT_CLEAR_ALL: u32 = 0x1_FFFF;

/// Additional words tagged onto the PC data payload by hardware.
pub const PC_DATA_EXTRA_AMOUNT: u32 = 4;

/// Special command offsets used on multi-core variants of the NPU.
pub const MULTICORE_COMMAND_OFFSETS: [usize; 2] = [0x1004, 0x3004];
