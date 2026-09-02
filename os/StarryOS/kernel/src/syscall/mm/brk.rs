use ax_task::current;
use linux_raw_sys::general::RLIMIT_DATA;

use crate::{
    StarryError, StarryResult,
    config::{USER_HEAP_SIZE, USER_HEAP_SIZE_MAX},
    mm::AddressSpaceMutationOutcome,
    task::AsThread,
};

pub fn sys_brk(addr: usize) -> StarryResult<isize> {
    let curr = current();
    let thread = curr.as_thread();
    let proc_data = &thread.proc_data;

    // Bind MM selection to exec's address-space swap. The actual brk scalar is
    // owned by AddrSpace and protected by its mutation lock, like Linux
    // `mm->brk` under `mmap_lock`; this outer lock only prevents selecting the
    // old MM while exec publishes a replacement.
    let _mm_transaction = loop {
        if let Some(guard) = proc_data.exec_lock.try_lock() {
            break guard;
        }
        if thread.has_exit_request() {
            return Err(StarryError::Interrupted);
        }
        ax_task::yield_now();
    };

    // Read process policy before taking the address-space lock. No MM path
    // takes rlim after entering an opposite lock order.
    let rlimit_data = proc_data.rlim.read()[RLIMIT_DATA].current;
    let aspace_pin = proc_data.pin_aspace()?;
    let mut aspace = aspace_pin.lock();
    let current_top = aspace.heap_break();

    // brk(0) is an MM query and must observe the same MM/scalar publication as
    // expansion and shrink.
    if addr == 0 {
        return Ok(current_top as isize);
    }

    // Linux brk syscall semantics:
    // - Success: return new break address
    // - Failure: return current break address (NOT -1, no errno)

    // Check address is within valid heap range
    let heap_start = aspace.heap_start();
    let Some(heap_end) = heap_start.checked_add(USER_HEAP_SIZE_MAX) else {
        return Ok(current_top as isize);
    };
    if !(heap_start..=heap_end).contains(&addr) {
        return Ok(current_top as isize);
    }

    // Linux v7.1 `check_data_rlimit()` applies the byte-precise limit before
    // page alignment: (new_brk - start_brk) + (end_data - start_data).
    // RLIM_INFINITY (u64::MAX) means unlimited.
    if rlimit_data != u64::MAX {
        let Some(heap_size) = addr.checked_sub(heap_start) else {
            return Ok(current_top as isize);
        };
        let Some(data_size) = aspace.executable_data_size() else {
            return Ok(current_top as isize);
        };
        let exceeds_limit = u64::try_from(heap_size)
            .ok()
            .and_then(|heap| {
                u64::try_from(data_size)
                    .ok()
                    .and_then(|data| heap.checked_add(data))
            })
            .is_none_or(|usage| usage > rlimit_data);
        if exceeds_limit {
            return Ok(current_top as isize);
        }
    }

    // Initial heap region end address (already mapped during ELF loading)
    let Some(initial_heap_end) = heap_start.checked_add(USER_HEAP_SIZE) else {
        return Ok(current_top as isize);
    };

    match aspace.resize_heap_break(addr, initial_heap_end) {
        Ok(AddressSpaceMutationOutcome::Complete) => Ok(addr as isize),
        Ok(AddressSpaceMutationOutcome::PublishedPendingTlb(error)) => {
            // Publication cannot be rolled back while a target CPU may retain
            // the old translation. The typed receipt owns the pending work and
            // the matching break is already visible in this MM.
            Err(error)
        }
        // Linux brk reports an ordinary unpublished failure by returning the
        // old break, without setting errno.
        Err(_) => Ok(current_top as isize),
    }
}
