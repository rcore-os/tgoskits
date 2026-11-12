//! LoongArch64 native EFI preparation and compatibility policy.

pub(in crate::loader) const ALLOW_IDENTITY_COUNTER_FALLBACK: bool = true;
pub(in crate::loader) const RETRY_IDENTITY_FAILURE: bool = true;

pub(in crate::loader) fn run() -> uefi::Status {
    super::super::tcp4::run(None)
}

/// Makes bytes written by the loader visible to the target instruction stream.
pub(in crate::loader) fn flush_instruction_cache() {
    // SAFETY: The barriers have no memory operands and do not touch the stack.
    // They order the completed image writes before the following EFI entry call.
    unsafe {
        core::arch::asm!("dbar 0", "ibar 0", options(nostack));
    }
}
