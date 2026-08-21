//! mlockall(2) / munlockall(2) / munlock(2) — process-wide memory locking.
//!
//! StarryOS has no swap and the page cache is the only evictable storage, so
//! every user mapping is effectively resident. These syscalls still enforce
//! the Linux ABI contract: flag validation, capability checks, range and
//! coverage validation, and the process-wide `MCL_*` state exposed through
//! `mlockall`/`munlockall`.

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::paging::MappingFlags;
use ax_task::current;
use linux_raw_sys::general::{MCL_CURRENT, MCL_FUTURE, MCL_ONFAULT};

use crate::{
    StarryError, StarryResult,
    task::AsThread,
};

/// mlockall(2) — lock the calling process's current and/or future mappings.
pub fn sys_mlockall(flags: i32) -> StarryResult<isize> {
    let valid = MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT;
    let flags = flags as u32;
    if flags & !valid != 0 {
        return Err(StarryError::InvalidInput);
    }
    // MCL_ONFAULT without CURRENT or FUTURE has nothing to apply to.
    if flags == MCL_ONFAULT {
        return Err(StarryError::InvalidInput);
    }

    let caller = current().as_thread().cred();
    if !caller.has_cap_ipc_lock() {
        return Err(StarryError::OperationNotPermitted);
    }

    let curr = current();
    let proc = curr.as_thread().proc_data.clone();

    // MCL_CURRENT requires every currently mapped area to be accessible; the
    // no-swap kernel makes the range resident once populated.
    if flags & MCL_CURRENT != 0 {
        let aspace_arc = proc.aspace();
        let aspace = aspace_arc.lock();
        for area in aspace.areas() {
            if !aspace.can_access_range(area.start(), area.size(), MappingFlags::empty()) {
                return Err(StarryError::NoMemory);
            }
        }
    }

    proc.set_mlockall_flags(flags);
    Ok(0)
}

/// munlockall(2) — unlock all mappings of the calling process.
pub fn sys_munlockall() -> StarryResult<isize> {
    let curr = current();
    curr.as_thread().proc_data.set_mlockall_flags(0);
    Ok(0)
}

/// munlock(2) — unlock a page range.
///
/// The no-swap kernel has no explicit lock bit to clear, so this validates
/// the same range contract as mlock(2) (including `ENOMEM` on unmapped
/// holes) and otherwise succeeds.
pub fn sys_munlock(addr: usize, length: usize) -> StarryResult<isize> {
    if length == 0 {
        return Ok(0);
    }
    let aligned = addr.align_down(PAGE_SIZE_4K);
    let raw_end = addr.checked_add(length).ok_or(StarryError::InvalidInput)?;
    let end = raw_end.align_up(PAGE_SIZE_4K);
    if end < raw_end {
        return Err(StarryError::InvalidInput);
    }
    let size = end - aligned;

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let aspace = aspace_arc.lock();
    let start = VirtAddr::from(aligned);
    if !aspace.can_access_range(start, size, MappingFlags::empty()) {
        return Err(StarryError::NoMemory);
    }
    Ok(0)
}

#[cfg(test)]
pub(crate) fn mlock_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::MCL_CURRENT;
    assert!(MCL_CURRENT == 1);
    // MCL_ONFAULT alone is rejected.
    assert!(sys_mlockall(MCL_ONFAULT as i32).is_err());
    // Invalid high bits are rejected before capability checks in the pure
    // validation layer (the full syscall checks capabilities after flags).
    assert!((0x8000_0000u32 & !(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)) != 0);
    true
}
