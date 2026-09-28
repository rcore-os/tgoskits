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
