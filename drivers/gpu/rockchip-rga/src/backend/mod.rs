//! Generation-specific RGA hardware backends behind one trait (spec §20.1).
pub mod rga2;
pub mod rga3;

use crate::{RgaHardwareVersion, RgaVersion, error::Result, operation::RgaOperation};

/// Hardware completion status, polled out of hard-IRQ context in PR-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgaStatus {
    Busy,
    Done,
    Error,
}

/// One-shot snapshot of an RGA core's engine state, captured on a timeout so a board run can
/// localize whether the engine never started, errored, or completed on a different bit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RgaDiag {
    pub int: u32,
    pub sys_ctrl: u32,
    pub cmd_ctrl: u32,
    pub cmd_base: u32,
    pub status: u32,
    pub version: u32,
    pub cmd_phys: u64,
}

/// A generation-specific RGA core controller. Owns its MMIO region and DMA context.
pub trait RgaBackend: Send {
    fn generation(&self) -> RgaVersion;
    fn read_version(&self) -> RgaHardwareVersion;
    /// Returns Ok(()) if this backend can execute `op`, else `RgaError::Unsupported`.
    fn supports(&self, op: &RgaOperation) -> Result<()>;
    /// Program registers/command for a validated `op` and start the engine (non-blocking).
    fn submit(&mut self, op: &RgaOperation) -> Result<()>;
    /// Poll hardware completion (non-blocking).
    fn poll(&self) -> RgaStatus;
    /// Acknowledge/clear the interrupt/status after a completed or errored job.
    fn ack(&mut self);
    /// Reset the core for recovery (timeout/fatal error).
    fn reset(&mut self) -> Result<()>;
    /// Read-only engine-state snapshot for diagnostics (default zeroed for backends without MMIO).
    fn diag(&self) -> RgaDiag {
        RgaDiag::default()
    }
}
