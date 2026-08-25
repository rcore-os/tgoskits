//! AArch64 hypervisor physical-GIC interrupt ingress.

/// Fixed IRQ-safe ingress implemented by the hypervisor runtime.
///
/// The dynamic platform queries ownership after acknowledging a GIC SPI, then
/// publishes only after the priority drop. Implementations must not allocate,
/// look up a VM, acquire ordinary locks, or invoke subscribers.
#[ax_crate_interface::def_interface]
pub trait Aarch64HvIrqSink {
    /// Returns whether `intid` has a pre-registered guest route.
    ///
    /// This is a race-tolerant hint. The caller must retain a fallback
    /// completion until [`Self::publish_physical_gic_claim`] accepts the claim.
    fn has_assigned_physical_spi(intid: u32) -> bool;

    /// Publishes one detached physical GIC SPI claim to its guest route.
    fn publish_physical_gic_claim(intid: u32) -> bool;
}

/// Returns whether one physical GIC SPI currently has a guest route.
#[inline]
pub fn has_assigned_physical_spi(intid: u32) -> bool {
    ax_crate_interface::call_interface!(Aarch64HvIrqSink::has_assigned_physical_spi, intid)
}

/// Publishes one detached physical GIC SPI claim to the hypervisor runtime.
#[inline]
pub fn publish_physical_gic_claim(intid: u32) -> bool {
    ax_crate_interface::call_interface!(Aarch64HvIrqSink::publish_physical_gic_claim, intid)
}
