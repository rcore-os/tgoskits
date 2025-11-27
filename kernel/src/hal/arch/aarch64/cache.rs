use memory_addr::VirtAddr;

use crate::hal::CacheOp;

impl From<CacheOp> for aarch64_cpu_ext::cache::CacheOp {
    fn from(op: CacheOp) -> Self {
        match op {
            CacheOp::Clean => aarch64_cpu_ext::cache::CacheOp::Clean,
            CacheOp::Invalidate => aarch64_cpu_ext::cache::CacheOp::Invalidate,
            CacheOp::CleanAndInvalidate => aarch64_cpu_ext::cache::CacheOp::CleanAndInvalidate,
        }
    }
}

pub fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
    aarch64_cpu_ext::cache::dcache_range(op.into(), addr.as_usize(), size);
}
