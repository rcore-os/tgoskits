use ax_memory_addr::VirtAddr;

#[allow(unused)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum CacheOp {
    /// Write back to memory
    Clean,
    /// Invalidate cache
    Invalidate,
    /// Clean and invalidate
    CleanAndInvalidate,
}

pub fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
    crate::host::arch::cache::dcache_range(op, addr, size);
}
