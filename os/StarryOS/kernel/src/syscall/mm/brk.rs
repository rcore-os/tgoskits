use axerrno::AxResult;
use axhal::paging::{MappingFlags, PageSize};
use axtask::current;
use memory_addr::{VirtAddr, align_up_4k};

use crate::{
    config::{USER_HEAP_BASE, USER_HEAP_SIZE, USER_HEAP_SIZE_MAX},
    mm::Backend,
    task::AsThread,
};

pub fn sys_brk(addr: usize) -> AxResult<isize> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let current_top = proc_data.get_heap_top() as usize;
    let heap_limit = USER_HEAP_BASE + USER_HEAP_SIZE_MAX;

    if addr == 0 {
        return Ok(current_top as isize);
    }

    if addr < USER_HEAP_BASE || addr > heap_limit {
        return Ok(current_top as isize);
    }

    let new_top_aligned = align_up_4k(addr);
    let current_top_aligned = align_up_4k(current_top);
    // Initial heap region end address (already mapped during ELF loading)
    let initial_heap_end = USER_HEAP_BASE + USER_HEAP_SIZE;

    // Only map new pages when expanding beyond already mapped region
    // Expansion start should be the greater of initial_heap_end and current_top_aligned
    if new_top_aligned > current_top_aligned {
        let expand_start = VirtAddr::from(initial_heap_end.max(current_top_aligned));
        let expand_size = new_top_aligned.saturating_sub(expand_start.as_usize());

        if expand_size > 0
            && proc_data
                .aspace
                .lock()
                .map(
                    expand_start,
                    expand_size,
                    MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
                    false,
                    Backend::new_alloc(expand_start, PageSize::Size4K),
                )
                .is_err()
        {
            return Ok(current_top as isize);
        }
    } else if new_top_aligned < current_top_aligned {
        // Only unmap pages beyond the initially mapped heap region.
        let shrink_start = VirtAddr::from(initial_heap_end.max(new_top_aligned));
        let shrink_size = current_top_aligned.saturating_sub(shrink_start.as_usize());

        if shrink_size > 0
            && proc_data
                .aspace
                .lock()
                .unmap(shrink_start, shrink_size)
                .is_err()
        {
            return Ok(current_top as isize);
        }
    }

    proc_data.set_heap_top(addr);
    Ok(addr as isize)
}
