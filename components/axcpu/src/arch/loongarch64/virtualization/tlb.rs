//! Root-mode invalidation of cached guest translations.

/// Orders table writes and invalidates every locally cached guest GVA/GPA
/// translation, including entries tagged with a different guest ID.
///
/// # Safety
/// Execute in root mode on an LVZ CPU with local interrupts disabled. Retain
/// affected backing memory until remote guests have exited and cannot reenter
/// without an equivalent local invalidation. This operation is not a broadcast.
pub unsafe fn invalidate_guest_translations() {
    // SAFETY: the caller owns the local root-mode translation window.
    // INVTLB_ALLGID (0x12), as used by Linux LoongArch KVM, targets both guest
    // translation stages; ordinary host INVTLB operations select another domain.
    unsafe {
        core::arch::asm!(
            "dbar 0",
            "invtlb 0x12, $r0, $r0",
            "dbar 0",
            options(nostack)
        );
    }
}
