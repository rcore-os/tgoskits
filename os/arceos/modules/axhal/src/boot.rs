//! Boot-time metadata exposed through boot-protocol-agnostic accessors.

/// Returns kernel boot arguments when the active boot path provides them.
///
/// The facade keeps runtime users independent from whether the arguments came
/// from FDT, UEFI load options, ACPI-related firmware data, or another future
/// boot protocol. The current implementation falls back to FDT
/// `/chosen/bootargs`.
pub fn bootargs() -> Option<&'static str> {
    #[cfg(not(any(test, feature = "host-test")))]
    if let Some(bootargs) = axplat_dyn::bootargs() {
        return Some(bootargs);
    }

    crate::dtb::get_chosen_bootargs()
}

/// The validated, reserved physical range of the host archive, if provided.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitramfsRange {
    pub start: usize,
    pub end: usize,
    pub reclaimable: bool,
}

#[cfg(not(any(test, feature = "host-test")))]
pub fn initramfs_range() -> Option<InitramfsRange> {
    axplat_dyn::initramfs_range().map(|range| InitramfsRange {
        start: range.start,
        end: range.end,
        reclaimable: range.reclaimable,
    })
}

#[cfg(any(test, feature = "host-test"))]
pub fn initramfs_range() -> Option<InitramfsRange> {
    None
}

/// Claims the reserved external archive for unpacking and eventual reclamation.
pub fn take_initramfs_range() -> Option<Initramfs> {
    #[cfg(not(any(test, feature = "host-test")))]
    {
        axplat_dyn::take_initramfs_range().map(|inner| Initramfs { inner })
    }
    #[cfg(any(test, feature = "host-test"))]
    {
        None
    }
}
/// Unique archive ownership, with the platform lifetime kept inside the HAL.
#[derive(Debug)]
pub struct Initramfs {
    #[cfg(not(any(test, feature = "host-test")))]
    inner: axplat_dyn::Initramfs,
    #[cfg(any(test, feature = "host-test"))]
    unavailable: core::convert::Infallible,
}
impl Initramfs {
    pub fn bytes(&self) -> &[u8] {
        cfg_select! {
            any(test, feature = "host-test") => match self.unavailable {},
            _ => self.inner.bytes(),
        }
    }
    pub fn finish(self) -> Option<InitramfsReclaim> {
        cfg_select! {
            any(test, feature = "host-test") => match self.unavailable {},
            _ => self.inner.finish().map(|inner| InitramfsReclaim { inner }),
        }
    }
}
/// One-shot transfer of complete archive pages to the general allocator.
#[derive(Debug)]
pub struct InitramfsReclaim {
    #[cfg(not(any(test, feature = "host-test")))]
    inner: axplat_dyn::InitramfsReclaim,
    #[cfg(any(test, feature = "host-test"))]
    unavailable: core::convert::Infallible,
}
impl InitramfsReclaim {
    pub fn range(&self) -> ax_memory_addr::PhysAddrRange {
        cfg_select! {
            any(test, feature = "host-test") => match self.unavailable {},
            _ => {
                let range = self.inner.range();
                ax_memory_addr::PhysAddrRange::from_start_size(
                    (range.start as usize).into(),
                    range.len as usize,
                )
            }
        }
    }
    /// # Safety
    /// The complete range must have been successfully transferred to the allocator.
    pub unsafe fn complete(self) {
        cfg_select! {
            any(test, feature = "host-test") => match self.unavailable {},
            _ => {
                // SAFETY: forwarded from the caller's successful transfer proof.
                unsafe { self.inner.complete() };
            }
        }
    }
}

/// Returns the trusted firmware seed captured during early boot.
pub fn boot_entropy() -> Option<[u8; 32]> {
    #[cfg(not(any(test, feature = "host-test")))]
    {
        axplat_dyn::boot_entropy()
    }

    #[cfg(any(test, feature = "host-test"))]
    {
        None
    }
}

#[cfg(not(any(test, feature = "host-test")))]
pub use axplat_dyn::{ExecutionSnapshot, HandoffError, PhysicalRange};
