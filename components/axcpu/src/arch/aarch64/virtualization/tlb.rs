//! Guest translation-cache maintenance, separate from host EL2 stage one.

/// Invalidates all guest stage-one and stage-two translations in the
/// inner-shareable domain, across VMIDs, and waits for completion.
///
/// # Safety
/// Execute at non-VHE EL2 in the owning security state. The VM memory owner
/// must serialize descriptor updates and retain old backing memory until the
/// invalidation completes. Other CPUs may run guests in this shareable domain.
/// This does not stop a guest or grant ownership of a concurrently accessed page.
pub unsafe fn invalidate_guest_translations_inner_shareable() {
    // SAFETY: the caller owns the descriptor publication and retirement window.
    unsafe {
        core::arch::asm!("dsb ishst; tlbi alle1is; dsb ish; isb", options(nostack));
    }
}

/// Invalidates the current VTTBR's guest translations on this CPU.
///
/// # Safety
/// Execute at non-VHE EL2 while retaining the current VTTBR/VTCR context and
/// serializing its descriptor stores. Remote CPUs require the owner's separate
/// broadcast or rendezvous; this operation completes only local translations.
pub unsafe fn invalidate_current_guest_translations() {
    // SAFETY: the caller pins the active guest translation regime.
    unsafe {
        core::arch::asm!("dsb ish; tlbi vmalls12e1; dsb ish; isb", options(nostack));
    }
}
