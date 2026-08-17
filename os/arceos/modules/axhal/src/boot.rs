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

#[cfg(test)]
mod tests {
    #[test]
    fn bootargs_facade_is_available() {
        crate::dtb::init(0);

        assert_eq!(super::bootargs(), None);
    }

    #[test]
    fn boot_entropy_is_unavailable_without_firmware() {
        assert_eq!(super::boot_entropy(), None);
    }
}
