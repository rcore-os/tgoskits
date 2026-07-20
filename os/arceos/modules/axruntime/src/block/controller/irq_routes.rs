//! Owner-thread IRQ action transitions for exclusive device handoff.

use super::{
    BlockController, BlockHandoffError,
    source::{RuntimeIrqSource, quiesce_after_device_masked},
};

/// Masks the device, drains every local action, and retains each callback in a
/// detached token for guest return. Partial failure leaves the exact active or
/// detached state in `sources`; the owner keeps that state quarantined.
pub(super) fn detach_host_actions(
    controller: &BlockController,
    sources: &mut [RuntimeIrqSource],
) -> Result<(), BlockHandoffError> {
    controller.with_driver_endpoint_on_owner(|device| device.disable_irq())?;
    quiesce_after_device_masked(sources)?;
    for source in sources.iter_mut() {
        source.detach()?;
    }
    Ok(())
}

/// Restores every retained callback as a disabled action on the same owner CPU.
pub(super) fn reattach_host_actions(
    sources: &mut [RuntimeIrqSource],
) -> Result<(), BlockHandoffError> {
    for source in sources {
        source.reattach()?;
    }
    Ok(())
}
