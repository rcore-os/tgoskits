use ax_errno::AxResult;
use ax_memory_addr::{VirtAddr, align_up_4k};
use ax_runtime::hal::paging::{MappingFlags, PageSize};
use ax_task::current;
use linux_raw_sys::general::{RLIMIT_AS, RLIMIT_DATA};

use crate::{
    config::{USER_HEAP_BASE, USER_HEAP_SIZE, USER_HEAP_SIZE_MAX},
    mm::Backend,
    task::AsThread,
};

pub fn sys_brk(addr: usize) -> AxResult<isize> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    let current_top = proc_data.get_heap_top() as usize;

    // brk(0) returns current heap top
    if addr == 0 {
        return Ok(current_top as isize);
    }

    // Linux brk syscall semantics:
    // - Success: return new break address
    // - Failure: return current break address (NOT -1, no errno)

    // Check address is within valid heap range
    if !(USER_HEAP_BASE..=USER_HEAP_BASE + USER_HEAP_SIZE_MAX).contains(&addr) {
        return Ok(current_top as isize);
    }

    // Check RLIMIT_DATA: Linux limits heap expansion by RLIMIT_DATA.
    // The limit applies to (new_brk - start_brk) + (end_data - start_data).
    // Since we don't have end_data - start_data, we approximate by checking
    // (addr - USER_HEAP_BASE) against the soft limit.
    // RLIM_INFINITY (u64::MAX) means unlimited.
    let limits = proc_data.rlim.read();
    let rlimit_data = limits[RLIMIT_DATA].current;
    let rlimit_as = limits[RLIMIT_AS].current;
    drop(limits);
    if rlimit_data != u64::MAX {
        let heap_size = addr.saturating_sub(USER_HEAP_BASE);
        if heap_size > rlimit_data as usize {
            return Ok(current_top as isize);
        }
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

        if expand_size > 0 {
            let aspace_arc = proc_data.aspace();
            let mut aspace = aspace_arc.lock();
            let current_bytes = aspace
                .vm_stat
                .vss_pages()
                .checked_mul(ax_memory_addr::PAGE_SIZE_4K as u64);
            if current_bytes.is_none_or(|current_bytes| {
                starry_mm::admit_address_space(current_bytes, 0, expand_size as u64, rlimit_as)
                    .is_err()
            }) {
                return Ok(current_top as isize);
            }
            if aspace
                .map(
                    expand_start,
                    expand_size,
                    MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER,
                    false,
                    Backend::new_alloc(expand_start, PageSize::Size4K, "[heap]"),
                )
                .is_err()
            {
                return Ok(current_top as isize);
            }
            drop(aspace);
        }
    } else if new_top_aligned < current_top_aligned {
        // Only unmap pages beyond the initially mapped heap region.
        let shrink_start = VirtAddr::from(initial_heap_end.max(new_top_aligned));
        let shrink_size = current_top_aligned.saturating_sub(shrink_start.as_usize());

        if shrink_size > 0
            && proc_data
                .aspace()
                .lock()
                .unmap(shrink_start, shrink_size)
                .is_err()
        {
            return Ok(current_top as isize);
        }
    }

    proc_data.set_heap_top(addr);
    Ok(addr as isize) // Linux brk syscall returns new break address on success
}
