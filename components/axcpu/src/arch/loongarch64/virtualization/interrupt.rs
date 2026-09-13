//! Local LVZ interrupt-register operations.

use super::vcpu::LocalIrqs;
use crate::registers::{read_csr, read_guest_csr, write_csr, write_guest_csr};

/// One of the thirteen architectural local guest interrupt sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestInterrupt(u8);

impl GuestInterrupt {
    /// Validates a local guest interrupt source number.
    pub const fn new(source: u8) -> Option<Self> {
        if source <= 12 {
            Some(Self(source))
        } else {
            None
        }
    }

    /// Returns the architectural source number.
    pub const fn source(self) -> u8 {
        self.0
    }

    /// Pulses a hardware source or sets a software guest ESTAT pending bit.
    ///
    /// # Safety
    /// The caller must own the bound guest's local register bank and keep the
    /// current CPU pinned. This operation must not race another guest owner.
    /// Hardware passthrough must be disabled while pulsing software-owned inputs.
    pub unsafe fn pulse(self) {
        let _irqs = LocalIrqs::disable();
        // SAFETY: the pinned guest owner excludes conflicting register access.
        unsafe {
            if (2..=9).contains(&self.0) {
                let bit = 1usize << (self.0 - 2);
                let current = read_csr::<0x52>();
                // Guest ESTAT contains the effective pending inputs. HWIS is
                // an injection field, not a durable pending-state snapshot;
                // the supported LVZ model reads its low bits as zero.
                let pending = (read_guest_csr::<5>() >> 2) & 0xff;
                let cleared = (current & !0xff) | (pending & !bit);
                write_csr::<0x52>(cleared);
                write_csr::<0x52>(cleared | bit);
            } else {
                let current = read_guest_csr::<5>();
                write_guest_csr::<5>(current | (1usize << self.0));
            }
        }
    }
}

/// Replaces the pending mask for the guest's eight hardware interrupt inputs.
///
/// # Safety
/// The caller must own the local guest bank and stay on its CPU through return.
pub unsafe fn set_hwi_pending(mask: u8) {
    let _irqs = LocalIrqs::disable();
    // SAFETY: the caller owns this local bank; IRQs exclude an interrupted RMW.
    unsafe {
        let current = read_csr::<0x52>();
        write_csr::<0x52>((current & !0xff) | usize::from(mask));
    }
}

/// Selects which physical hardware inputs are passed through to the guest.
///
/// # Safety
/// The caller must own the local guest bank and the passed-through interrupt
/// sources, quiesce physical inputs across this routing change, and stay on the
/// owning CPU throughout the operation.
pub unsafe fn set_hwi_passthrough(mask: u8) {
    let _irqs = LocalIrqs::disable();
    // SAFETY: the owner supplies the hardware routing policy. Preserve pending
    // HWIS bits; clearing them here could lose a guest's outstanding interrupt.
    unsafe {
        let current = read_csr::<0x52>();
        let pending = (read_guest_csr::<5>() >> 2) & 0xff;
        write_csr::<0x52>((current & !0xffff) | pending | (usize::from(mask) << 8));
    }
}
