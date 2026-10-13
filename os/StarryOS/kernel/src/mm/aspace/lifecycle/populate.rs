//! Explicit prefaulting through the same publication path as hardware faults.

use ax_memory_addr::{PAGE_SIZE_4K, VirtAddrRange};
use ax_runtime::hal::paging::MappingFlags;

use super::{FaultResult, MmPin, PageFaultFlags};
use crate::{StarryError, StarryResult};

#[derive(Clone, Copy)]
pub(crate) enum PopulateAccess {
    Read,
    Write,
}

impl PopulateAccess {
    fn fault_flags(self) -> PageFaultFlags {
        match self {
            Self::Read => PageFaultFlags::READ,
            Self::Write => PageFaultFlags::WRITE,
        }
    }
}

impl MmPin {
    /// Prefaults a page-aligned range without hiding failures or rolling back
    /// its completed prefix. Unlike user access, holes mean ENOMEM, invalid
    /// mapping permissions mean EINVAL, and a backing fault means EFAULT
    /// rather than delivery of SIGBUS/SIGSEGV to the calling task.
    pub(crate) fn populate_advised_range(
        &self,
        range: VirtAddrRange,
        access: PopulateAccess,
        interrupted: impl Fn() -> bool,
    ) -> StarryResult {
        let fault_flags = access.fault_flags();
        let required = MappingFlags::from(fault_flags);
        let mut cursor = range.start;
        while cursor < range.end {
            if interrupted() {
                return Err(StarryError::Interrupted);
            }
            let plan = {
                let aspace = self.0.aspace.lock();
                let entry = aspace
                    .vma_root
                    .lookup_entry(cursor)
                    .ok_or(StarryError::NoMemory)?;
                // Linux prefault admission uses the requested VMA access,
                // not the readable PTE permission added for PROT_WRITE-only
                // mappings by the architecture's hardware permission rules.
                if entry.operation().is_linear() || !entry.reported_rights().contains(required) {
                    return Err(StarryError::InvalidInput);
                }
                // Admission and the VMA/PTE snapshot share one lock hold.
                // Preparation may drop it, but apply validates the captured
                // epoch; a retry must recheck these admission rules too.
                aspace.plan_page_fault(cursor, fault_flags, self.0.transparent_huge_page_mode())
            };
            let result = match plan {
                Ok(plan) => self.resolve_page_fault_plan(plan, cursor),
                Err(result) => result,
            };
            match result {
                FaultResult::Handled => cursor += PAGE_SIZE_4K,
                FaultResult::Retry => crate::task::yield_now(),
                FaultResult::NoMemory => return Err(StarryError::NoMemory),
                FaultResult::PermissionDenied => return Err(StarryError::InvalidInput),
                FaultResult::Unmapped | FaultResult::Sigbus(_) => {
                    return Err(StarryError::BadAddress);
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use ax_memory_addr::VirtAddr;

    use super::*;
    use crate::mm::{AddrSpace, MappingOperation, MmHandle};

    #[axtest::axtest]
    fn explicit_write_population_breaks_cow_before_userspace_writes() {
        let start = VirtAddr::from_usize(0x7400_0000);
        let range = VirtAddrRange::from_start_size(start, PAGE_SIZE_4K);
        let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
        let mut parent = AddrSpace::new_empty(start, PAGE_SIZE_4K).unwrap();
        parent
            .map(
                start,
                PAGE_SIZE_4K,
                flags,
                true,
                MappingOperation::new_alloc(start, PAGE_SIZE_4K, "[populate-cow]"),
            )
            .unwrap();
        let parent_frame = parent.pt.query(start).unwrap().0;
        let child = parent.try_clone().unwrap();
        let handle = MmHandle::from_arc(child.clone()).unwrap();
        let pin = handle.pin().unwrap();
        pin.populate_advised_range(range, PopulateAccess::Read, || false)
            .unwrap();
        assert_eq!(child.lock().pt.query(start).unwrap().0, parent_frame);
        assert!(
            !child
                .lock()
                .pt
                .query(start)
                .unwrap()
                .1
                .contains(MappingFlags::WRITE)
        );
        assert!(matches!(
            pin.populate_advised_range(range, PopulateAccess::Write, || true),
            Err(StarryError::Interrupted)
        ));
        assert_eq!(child.lock().pt.query(start).unwrap().0, parent_frame);
        pin.populate_advised_range(range, PopulateAccess::Write, || false)
            .unwrap();
        let (frame, flags, _) = child.lock().pt.query(start).unwrap();
        assert_ne!(frame, parent_frame);
        assert!(flags.contains(MappingFlags::WRITE));
        assert_eq!(parent.pt.query(start).unwrap().0, parent_frame);
        drop(pin);
        drop(child);
        handle.release_user_ref().unwrap().reclaim().unwrap();
        parent.reset_uninstalled_for_loader().unwrap();
    }
}
