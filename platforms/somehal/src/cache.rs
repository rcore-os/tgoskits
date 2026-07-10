//! Data-cache maintenance helpers exposed through SomeHAL.

pub use someboot::DCacheOp;

/// Maintains a data-cache range using the active platform implementation.
pub fn dcache_range(op: DCacheOp, addr: *const u8, size: usize) {
    someboot::mem::dcache_range(op, addr, size);
}

/// Prepares a cached range before it is remapped as uncached for DMA.
pub fn dma_coherent_before_make_uncached(addr: *const u8, size: usize) {
    someboot::mem::dma_coherent_before_make_uncached(addr, size);
}

/// Prepares an uncached DMA range before restoring cached mappings.
pub fn dma_coherent_before_restore_cached(addr: *const u8, size: usize) {
    someboot::mem::dma_coherent_before_restore_cached(addr, size);
}

/// Completes ordering after a DMA coherent mapping attribute update.
pub fn dma_coherent_after_mapping_update() {
    someboot::mem::dma_coherent_after_mapping_update();
}
