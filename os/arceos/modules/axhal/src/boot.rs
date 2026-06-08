//! Boot-time metadata exposed through boot-protocol-agnostic accessors.

/// Returns kernel boot arguments when the active boot path provides them.
///
/// The facade keeps runtime users independent from whether the arguments came
/// from FDT, UEFI load options, ACPI-related firmware data, or another future
/// boot protocol. The current implementation falls back to FDT
/// `/chosen/bootargs`.
pub fn bootargs() -> Option<&'static str> {
    crate::dtb::get_chosen_bootargs()
}
