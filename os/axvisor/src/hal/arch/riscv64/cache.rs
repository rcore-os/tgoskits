use ax_memory_addr::VirtAddr;

use axvisor_api::arch::CacheOp;

pub fn dcache_range(_op: CacheOp, _addr: VirtAddr, _size: usize) {}
