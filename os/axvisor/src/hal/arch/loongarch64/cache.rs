#![allow(unsafe_op_in_unsafe_fn)]

use ax_memory_addr::VirtAddr;

use crate::hal::CacheOp;

const CACHE_LINE_SIZE: usize = 64;
const DCACHE_INV: u8 = 0x18;
const DCACHE_WB: u8 = 0x19;
const DCACHE_WB_INV: u8 = 0x1B;

unsafe fn cache_range<const OP: u8>(addr: VirtAddr, size: usize) {
    let start = addr.as_usize() & !(CACHE_LINE_SIZE - 1);
    let end = addr.as_usize() + size;
    let mut current = start;

    while current < end {
        core::arch::asm!("cacop {0}, {1}, 0", const OP, in(reg) current);
        current += CACHE_LINE_SIZE;
    }
}

pub fn dcache_range(op: CacheOp, addr: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }

    unsafe {
        match op {
            CacheOp::Clean => cache_range::<DCACHE_WB>(addr, size),
            CacheOp::Invalidate => cache_range::<DCACHE_INV>(addr, size),
            CacheOp::CleanAndInvalidate => cache_range::<DCACHE_WB_INV>(addr, size),
        }
        core::arch::asm!("dbar 0");
    }
}
