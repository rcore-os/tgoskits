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
}
