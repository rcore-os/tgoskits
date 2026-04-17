//! Minimal job submission support translated from the C driver.
//!
//! The original C implementation wires into the Linux kernel's scheduling,
//! DMA, fence, and waitqueue infrastructure.  In this Rust port we keep the
//! data layout and validation logic but replace operating system specific
//! interactions with lightweight placeholders so the higher level driver code
//! can be compiled and exercised in a freestanding environment.

#![allow(dead_code)]

/// Maximum number of hardware cores supported by the IP.
pub const RKNPU_MAX_CORES: usize = 3;

/// Maximum number of sub-core task descriptors accepted per submit.
pub const RKNPU_MAX_SUBCORE_TASKS: usize = 5;

/// Automatic core selection requested by the caller.
pub const RKNPU_CORE_AUTO_MASK: u32 = 0x00;
/// Explicit mask targeting core 0.
pub const RKNPU_CORE0_MASK: u32 = 0x01;
/// Explicit mask targeting core 1.
pub const RKNPU_CORE1_MASK: u32 = 0x02;
/// Explicit mask targeting core 2.
pub const RKNPU_CORE2_MASK: u32 = 0x04;

/// Job flag requesting PC (Program Counter) mode.
pub const RKNPU_JOB_PC: u32 = 1 << 0;
/// Job flag requesting non-blocking submission.
pub const RKNPU_JOB_NONBLOCK: u32 = 1 << 1;
/// Job flag enabling ping-pong execution.
pub const RKNPU_JOB_PINGPONG: u32 = 1 << 2;
/// Job flag indicating a fence should be waited on before execution.
pub const RKNPU_JOB_FENCE_IN: u32 = 1 << 3;
/// Job flag indicating a fence should be signalled on completion.
pub const RKNPU_JOB_FENCE_OUT: u32 = 1 << 4;

/// Task descriptor consumed by the hardware command parser in PC mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, packed)]
pub struct RknpuTask {
    pub flags: u32,
    pub op_idx: u32,
    pub enable_mask: u32,
    pub int_mask: u32,
    pub int_clear: u32,
    pub int_status: u32,
    pub regcfg_amount: u32,
    pub regcfg_offset: u32,
    pub regcmd_addr: u64,
}

/// High level view of a sub-core task request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct RknpuSubcoreTask {
    pub task_start: u32,
    pub task_number: u32,
}

bitflags::bitflags! {
    /// Internal job submission flags.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct  JobMode: u32 {
        const SLAVE =  0;
        const PC = 1 << 0;
        const BLOCK = 0 << 1;
        const NONBLOCK = 1 << 1;
        const PINGPONG = 1 << 2;
        const FENCE_IN = 1 << 3;
        const FENCE_OUT = 1 << 4;
    }
}

/// Helper calculating the mask for the given core index.
pub const fn core_mask_from_index(index: usize) -> u32 {
    match index {
        0 => RKNPU_CORE0_MASK,
        1 => RKNPU_CORE1_MASK,
        2 => RKNPU_CORE2_MASK,
        _ => 0,
    }
}

/// Counts how many cores are enabled in the provided mask.
pub const fn core_count_from_mask(mask: u32) -> u32 {
    mask.count_ones()
}
