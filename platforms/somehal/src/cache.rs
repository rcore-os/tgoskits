//! Data-cache maintenance helpers exposed through SomeHAL.

pub use someboot::DCacheOp;

/// Maintains a data-cache range using the active platform implementation.
pub fn dcache_range(op: DCacheOp, addr: *const u8, size: usize) {
    someboot::mem::dcache_range(op, addr, size);
}

/// Prepares cached pages before creating an uncached DMA alias.
pub fn dma_coherent_before_map_uncached(addr: *const u8, size: usize) {
    someboot::mem::dma_coherent_before_map_uncached(addr, size);
}

/// Orders accesses before removing an uncached DMA alias.
pub fn dma_coherent_before_unmap_uncached(addr: *const u8, size: usize) {
    someboot::mem::dma_coherent_before_unmap_uncached(addr, size);
}

/// Completes ordering after a DMA coherent alias update.
pub fn dma_coherent_after_mapping_update() {
    someboot::mem::dma_coherent_after_mapping_update();
}
