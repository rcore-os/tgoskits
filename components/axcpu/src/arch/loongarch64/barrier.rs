//! Page-table publication ordering.

/// Publishes page-table stores before a subsequent invalidation.
pub fn synchronize_page_table_writes() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

/// Completes prior memory accesses before a subsequent device doorbell.
pub fn data_fence() {
    // SAFETY: DBAR orders local memory operations without dereferencing memory.
    unsafe {
        core::arch::asm!("dbar 0", options(nostack));
    }
}
