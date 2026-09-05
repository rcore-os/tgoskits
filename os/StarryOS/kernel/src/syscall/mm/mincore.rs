// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2025 KylinSoft Co., Ltd. <https://www.kylinos.cn/>
// Copyright (C) 2025 Azure-stars <Azure_stars@126.com>
// Copyright (C) 2025 Yuekai Jia <equation618@gmail.com>
// See LICENSES for license details.
//
// This file has been modified by KylinSoft on 2025.

use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_task::current;
use starry_vm::vm_write_slice;

use crate::{StarryError, StarryResult, task::AsThread};

// A cache query owns a backend snapshot, so keep fewer entries than Linux
// needs for its byte-only scratch page. Neither batch buffer can allocate.
const MINCORE_BATCH_PAGES: usize = 32;

fn validate_mincore_request(
    addr: usize,
    length: usize,
    vec_is_null: bool,
    user_base: usize,
    user_end: usize,
) -> StarryResult<usize> {
    let start = VirtAddr::from(addr);
    if !start.is_aligned(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }

    // Linux treats a zero-page request as a no-op. In particular, no output
    // byte is touched, so a null `vec` cannot turn it into EFAULT.
    if length == 0 {
        return Ok(0);
    }

    let end = addr.checked_add(length).ok_or(StarryError::NoMemory)?;
    if addr < user_base || end > user_end {
        return Err(StarryError::NoMemory);
    }
    let pages = length.div_ceil(PAGE_SIZE_4K);
    if vec_is_null {
        return Err(StarryError::BadAddress);
    }
    Ok(pages)
}

/// Check whether pages are resident in memory.
///
/// The mincore() system call determines whether pages of the calling process's
/// virtual memory are resident in RAM.
///
/// # Arguments
/// * `addr` - Starting address (must be a multiple of the page size)
/// * `length` - Length of the region in bytes (effectively rounded up to next page boundary)
/// * `vec` - Output array containing at least (length+PAGE_SIZE-1)/PAGE_SIZE bytes.
///
/// # Return Value
/// * `Ok(0)` on success
/// * `Err(EAGAIN)` - Kernel is temporarily out of resources (not implemented in StarryOS)
/// * `Err(EFAULT)` - vec points to an invalid address (handled by vm_write_slice)
/// * `Err(EINVAL)` - addr is not a multiple of the page size
/// * `Err(ENOMEM)` - length is greater than (TASK_SIZE - addr), or negative length, or `addr` to `addr`+`length` contained unmapped memory
///
/// # Notes from Linux man page
/// - The least significant bit (bit 0) is set if page is resident in memory
/// - Bits 1-7 are reserved and currently cleared
/// - Information is only a snapshot; pages can be swapped at any moment
///
/// # Linux Errors
/// - EAGAIN:  kernel temporarily out of resources
/// - EFAULT: vec points to invalid address
/// - EINVAL: addr not page-aligned
/// - ENOMEM: length > (TASK_SIZE - addr), negative length, or unmapped memory
pub fn sys_mincore(addr: usize, length: usize, vec: *mut u8) -> StarryResult<isize> {
    let start_addr = VirtAddr::from(addr);
    let curr = current();
    let cred = curr.as_thread().cred();
    let aspace_pin = curr.as_thread().proc_data.pin_aspace()?;
    let (user_base, user_end) = {
        let aspace = aspace_pin.lock();
        (aspace.base().as_usize(), aspace.end().as_usize())
    };
    let page_count = validate_mincore_request(addr, length, vec.is_null(), user_base, user_end)?;

    debug!("sys_mincore <= addr: {addr:#x}, length: {length:#x}, vec: {vec:?}");

    if page_count == 0 {
        return Ok(0);
    }

    crate::mm::check_access(vec.addr(), page_count)?;
    let mut completed = 0;
    while completed < page_count {
        let batch_pages = (page_count - completed).min(MINCORE_BATCH_PAGES);
        let mut result = [0u8; MINCORE_BATCH_PAGES];
        let mut cache_queries = heapless::Vec::<_, MINCORE_BATCH_PAGES>::new();
        let mut filled = 0;
        let mut range_error = None;
        {
            let aspace = aspace_pin.lock();
            while filled < batch_pages {
                let address = start_addr + (completed + filled) * PAGE_SIZE_4K;
                let Some(probe) = aspace.mincore_probe(address) else {
                    range_error = Some(StarryError::NoMemory);
                    break;
                };
                // Residency is independent of access permission. In
                // particular, PROT_NONE does not relinquish a resident owner.
                if let Some(bytes) = aspace.resident_bytes_from(address) {
                    let pages = (bytes / PAGE_SIZE_4K).min(batch_pages - filled);
                    debug_assert!(pages != 0);
                    result[filled..filled + pages].fill(1);
                    filled += pages;
                } else {
                    if cache_queries.push((filled, address, probe)).is_err() {
                        unreachable!("one cache query per bounded output byte");
                    }
                    filled += 1;
                }
            }
        }

        // Cache lookups and copyout may take independent locks or fault.
        // Finish them after releasing MM metadata, then discard these owned
        // snapshots before beginning another bounded batch.
        for (index, address, probe) in cache_queries {
            if probe.mincore_resident(address, &cred) {
                result[index] = 1;
            }
        }
        if filled != 0 {
            vm_write_slice(vec.wrapping_add(completed), &result[..filled])?;
        }
        if let Some(error) = range_error {
            return Err(error);
        }
        completed += filled;
    }

    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn mincore_validation_rules_hold_for_test() -> bool {
    use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
    // Test mincore validation logic
    // Page-aligned address should pass alignment check
    let aligned_addr = VirtAddr::from(0x1000usize);
    assert!(aligned_addr.is_aligned(PAGE_SIZE_4K));

    // Non-page-aligned address should fail alignment check
    let unaligned_addr = VirtAddr::from(0x1001usize);
    assert!(!unaligned_addr.is_aligned(PAGE_SIZE_4K));

    // Zero address is aligned (0 is multiple of any page size)
    let zero_addr = VirtAddr::from(0usize);
    assert!(zero_addr.is_aligned(PAGE_SIZE_4K));

    // Test page count calculation
    let length: usize = 4096;
    let page_count = length.div_ceil(PAGE_SIZE_4K);
    assert!(page_count == 1);

    let length: usize = 8192;
    let page_count = length.div_ceil(PAGE_SIZE_4K);
    assert!(page_count == 2);

    let length: usize = 1;
    let page_count = length.div_ceil(PAGE_SIZE_4K);
    assert!(page_count == 1);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use crate::StarryError;

    #[test]
    fn mincore_validation_rules_hold() {
        assert!(super::mincore_validation_rules_hold_for_test());
    }

    #[test]
    fn zero_length_does_not_validate_output_pointer() {
        assert_eq!(
            super::validate_mincore_request(0x1000, 0, true, 0x1000, 0x20_0000).unwrap(),
            0
        );
    }

    #[test]
    fn overflowing_range_precedes_output_pointer_validation() {
        assert!(matches!(
            super::validate_mincore_request(
                usize::MAX & !(4096 - 1),
                4096,
                true,
                0x1000,
                0x20_0000,
            ),
            Err(StarryError::NoMemory)
        ));
    }
}
