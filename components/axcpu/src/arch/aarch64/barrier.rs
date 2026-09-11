//! Page-table publication ordering.

pub use super::asm::synchronize_page_table_writes;

/// Completes preceding explicit memory accesses throughout the system domain.
#[inline]
pub fn data_sync_system() {
    // SAFETY: the barrier changes ordering without dereferencing an address.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Synchronizes subsequent instructions with prior CPU control changes.
#[inline]
pub fn instruction_sync() {
    // SAFETY: instruction synchronization has no memory ownership side effects.
    unsafe { core::arch::asm!("isb", options(nostack)) };
}
