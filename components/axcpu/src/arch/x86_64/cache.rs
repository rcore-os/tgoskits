//! Local cache operations.

/// Serializes this CPU's instruction stream after coherent text publication.
/// Other executing CPUs require their own synchronized invocation.
pub fn flush_icache_all() {
    // CPUID is supported on every x86-64 CPU and serializes instruction fetch.
    // Compiler fences also order stores made through a different writable alias.
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    super::capability::cpuid(0, 0);
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Completes data-side preparation for subsequent local instruction synchronization.
/// This architecture needs no separate data-cache clean for coherent CPU stores;
/// callers must still perform `flush_icache_all` before executing modified text.
///
/// # Safety
/// The range must remain mapped; the caller coordinates concurrent text writes
/// and execution, including the subsequent instruction synchronization.
#[inline]
pub unsafe fn clean_dcache_range_to_pou(_range: crate::cache::CacheRange) {}
