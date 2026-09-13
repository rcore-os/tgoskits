//! SVM global interrupt flag controls.

/// Clears the SVM global interrupt flag on the current CPU.
///
/// # Safety
/// SVM must be enabled at ring 0. The caller must remain on this CPU and own
/// the corresponding guest/host transition until it restores GIF. Clearing
/// GIF also suppresses host events that ordinary RFLAGS.IF masking cannot.
#[inline]
pub unsafe fn clear_gif() {
    // SAFETY: the caller owns this enabled SVM CPU's transition interval.
    unsafe { core::arch::asm!("clgi", options(nostack, preserves_flags)) };
}

/// Sets the SVM global interrupt flag on the current CPU.
///
/// # Safety
/// SVM must be enabled at ring 0. All host register banks, TLS, extended state
/// and interrupt-entry storage must be restored before asynchronous host
/// events can be delivered. The caller must own this CPU's GIF transition.
#[inline]
pub unsafe fn set_gif() {
    // SAFETY: the caller has restored host state on the enabled SVM CPU.
    unsafe { core::arch::asm!("stgi", options(nostack, preserves_flags)) };
}
