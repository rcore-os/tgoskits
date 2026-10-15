//! Portable Command Queue Host Controller Interface (CQHCI) driver core.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::{boxed::Box, sync::Arc};
use core::ptr::NonNull;

pub use dma_api::DeviceDma;
pub use rdif_block::{BlkError, HardIrqHandler};
pub use sdmmc_protocol::sdio::host::SdMmcIrqHandle;

mod descriptor;
mod irq;
mod queue;
mod regs;

pub use queue::CqhciQueue;

/// Platform operations around the standardized CQHCI register block.
pub trait CqhciPlatform: Send + 'static {
    /// Monotonic elapsed time used to bound halt and reset transitions.
    fn now_ns(&self) -> u64;

    /// Programs vendor state required before CQHCI is enabled.
    fn prepare(&mut self, cqhci_base: usize) -> Result<(), BlkError>;

    /// Routes the SDHCI data path and shared IRQ to CQHCI.
    fn enable_data_path(&mut self) -> Result<(), BlkError>;

    /// Restores platform routing after CQHCI has stopped, including a failed
    /// enable. An error leaves restoration pending and may be retried during
    /// shutdown or drop. This does not disable CMDQ_MODE_EN on the card or
    /// authorize submitting legacy commands to the still-owned card session.
    fn disable_data_path(&mut self) -> Result<(), BlkError>;
}

/// Failed construction with the untouched legacy IRQ endpoint returned.
pub struct SetupError<L> {
    pub error: BlkError,
    pub legacy_irq: L,
}

impl<L> core::fmt::Debug for SetupError<L> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("SetupError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Constructs a CQHCI accelerator and the matching shared IRQ endpoint.
///
/// # Safety
///
/// `base` must cover the exclusively owned CQHCI register block belonging to
/// the same physical host as `dma`, `legacy_irq`, and `platform`. The DMA
/// capability must describe that host's address domain, coherency, and actual
/// addressing constraints. The register mapping must be aligned
/// to four bytes and cover 0x60 bytes of device MMIO. The mapping and device
/// clocks must remain live until both returned endpoints have been dropped.
/// The endpoints may run concurrently: the IRQ side touches only status
/// registers and the atomic mailbox, never descriptor or payload memory.
/// Once shutdown starts, the queue cannot be activated again.
pub unsafe fn queue_and_irq<L>(
    base: NonNull<u8>,
    dma: DeviceDma,
    platform: Box<dyn CqhciPlatform>,
    legacy_irq: L,
) -> Result<(CqhciQueue, Box<dyn HardIrqHandler>), SetupError<L>>
where
    L: SdMmcIrqHandle,
{
    // SAFETY: forwarded constructor contract covers the same live mapping.
    let (queue, irq_state) = match unsafe { CqhciQueue::new(base, dma, platform) } {
        Ok(parts) => parts,
        Err(error) => return Err(SetupError { error, legacy_irq }),
    };
    let irq = irq::CqhciIrqHandle::new(legacy_irq, Arc::clone(&irq_state));
    Ok((queue, Box::new(irq)))
}

pub(crate) fn read_u32(base: usize, offset: usize) -> u32 {
    // SAFETY: every caller holds the lifetime ownership established by the
    // constructor and accesses an aligned CQHCI register.
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

pub(crate) fn write_u32(base: usize, offset: usize, value: u32) {
    // SAFETY: every caller holds the lifetime ownership established by the
    // constructor and accesses an aligned CQHCI register.
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) }
}
