//! Page-table publication ordering.

/// Publishes page-table stores before a subsequent invalidation.
pub fn synchronize_page_table_writes() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

/// Completes prior loads and stores before later memory accesses.
/// This includes a compiler memory barrier and executes MFENCE locally.
pub fn data_fence() {
    // SAFETY: MFENCE is available in long mode and does not dereference memory.
    unsafe {
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}
