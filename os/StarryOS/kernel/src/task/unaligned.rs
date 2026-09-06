use ax_cpu::{UnalignedAccess, UnalignedAccessType, UnalignedError};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr};
use ax_runtime::hal::{cpu::trap::PageFaultFlags, paging::MappingFlags};

use super::Thread;
use crate::mm::AddrSpace;

pub(super) enum UnalignedEmulationResult {
    Complete,
    PageFault {
        address: VirtAddr,
        flags: PageFaultFlags,
    },
}

pub(super) fn emulate_user_unaligned(
    thread: &Thread,
    context: &mut ax_cpu::uspace::UserContext,
    fault_address: usize,
) -> Result<UnalignedEmulationResult, UnalignedError> {
    let access = unsafe { context.decode_unaligned_access_at(fault_address as u64)? };
    let flags = page_fault_flags(access.access_type());

    let result = if access.access_type() == UnalignedAccessType::Write {
        let Ok(aspace) = thread.proc_data.pin_aspace() else {
            return Ok(UnalignedEmulationResult::PageFault {
                address: VirtAddr::from_usize(fault_address),
                flags,
            });
        };
        let mut aspace = aspace.lock();
        if let Err(address) = prepare_write_range(&mut aspace, &access) {
            return Ok(UnalignedEmulationResult::PageFault { address, flags });
        }

        // Keep the address-space lock through the byte stores. The preflight
        // has populated every target page, and the lock prevents a concurrent
        // munmap/mprotect from reopening a check-to-commit fault window.
        unsafe { context.emulate_unaligned_access(access) }
    } else {
        unsafe { context.emulate_unaligned_access(access) }
    };

    match result {
        Ok(()) => Ok(UnalignedEmulationResult::Complete),
        Err(UnalignedError::PageFault(fault)) => Ok(UnalignedEmulationResult::PageFault {
            address: VirtAddr::from(fault.fault_address() as usize),
            flags: page_fault_flags(fault.access_type()),
        }),
        Err(err) => Err(err),
    }
}

fn page_fault_flags(access_type: UnalignedAccessType) -> PageFaultFlags {
    let access = match access_type {
        UnalignedAccessType::Read => PageFaultFlags::READ,
        UnalignedAccessType::Write => PageFaultFlags::WRITE,
    };
    access | PageFaultFlags::USER
}

fn prepare_write_range(aspace: &mut AddrSpace, access: &UnalignedAccess) -> Result<(), VirtAddr> {
    let start = VirtAddr::from(access.address() as usize);
    let Some(end_address) = access.address().checked_add(access.size() as u64) else {
        return Err(start);
    };
    let end = VirtAddr::from(end_address as usize);
    let mut current = start;

    while current < end {
        let page_start = current.align_down_4k();
        let Some(page_end) = page_start.checked_add(PAGE_SIZE_4K) else {
            return Err(current);
        };
        let segment_end = page_end.min(end);
        let segment_size = segment_end - current;

        if !aspace.can_access_range(current, segment_size, MappingFlags::WRITE) {
            return Err(current);
        }
        if aspace
            .populate_area(page_start, PAGE_SIZE_4K, MappingFlags::WRITE)
            .is_err()
        {
            return Err(current);
        }
        current = segment_end;
    }

    Ok(())
}
