//! PLIC memory map constants.
//!
//! This module defines all memory offsets and constants following the
//! RISC-V PLIC 1.0.0 specification.

// Strictly follows the PLIC 1.0.0 memory map as provided in riscv-plic-1.0.0.

/// Number of interrupt sources defined by PLIC 1.0.0.
/// Source IDs range from 1 to 1023 (inclusive). Source 0 is reserved and does not exist.
pub const PLIC_NUM_SOURCES: usize = 1024;

/// Offset to priority register for interrupt source 0 (reserved).
/// Priority for source N is at: PLIC_PRIORITY_OFFSET + N * 4
pub const PLIC_PRIORITY_OFFSET: usize = 0x000000;

/// Offset to the first pending register word (bits 0–31).
/// Word index W covers sources [W*32, W*32+31].
pub const PLIC_PENDING_OFFSET: usize = 0x001000;

/// Offset to the enable bits for context 0.
/// For context C, enable region starts at: PLIC_ENABLE_OFFSET + C * PLIC_ENABLE_STRIDE
pub const PLIC_ENABLE_OFFSET: usize = 0x002000;

/// Stride between contexts in the enable region (in bytes).
/// Each context uses 32 words = 128 bytes = 0x80.
pub const PLIC_ENABLE_STRIDE: usize = 0x80;

/// Offset to the control registers (threshold & claim/complete) for context 0.
/// For context C, control region starts at: PLIC_CONTEXT_CTRL_OFFSET + C * PLIC_CONTEXT_STRIDE
pub const PLIC_CONTEXT_CTRL_OFFSET: usize = 0x200000;

/// Stride between contexts in the control region (in bytes).
/// Each context uses two 32-bit registers (8 bytes), but spaced by 0x1000.
pub const PLIC_CONTEXT_STRIDE: usize = 0x1000;

/// Offset within a context's control region to the priority threshold register.
pub const PLIC_CONTEXT_THRESHOLD_OFFSET: usize = 0x00;

/// Offset within a context's control region to the claim/complete register.
pub const PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET: usize = 0x04;
