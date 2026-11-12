//! x86_64 native EFI preparation and compatibility policy.

pub(in crate::loader) const ALLOW_IDENTITY_COUNTER_FALLBACK: bool = true;
pub(in crate::loader) const RETRY_IDENTITY_FAILURE: bool = true;

pub(in crate::loader) fn run() -> uefi::Status {
    super::super::tcp4::run(None)
}

/// x86 uses coherent instruction and data caches for normal memory.
pub(in crate::loader) fn flush_instruction_cache() {}
