//! Page-table publication ordering.

/// Publishes page-table stores before a subsequent invalidation.
pub fn synchronize_page_table_writes() {
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
}

/// Completes ordering of ordinary loads and stores before a platform handoff.
pub fn data_fence() {
    // SAFETY: this fence orders memory accesses and does not change privilege.
    unsafe {
        core::arch::asm!("fence rw, rw", options(nostack));
    }
}
