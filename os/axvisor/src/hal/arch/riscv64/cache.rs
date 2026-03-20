use memory_addr::VirtAddr;

use crate::hal::CacheOp;

pub fn dcache_range(_op: CacheOp, _addr: VirtAddr, _size: usize) {}
