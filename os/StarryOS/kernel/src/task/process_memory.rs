//! Process address-space ownership and exit-time release.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_sync::{PiMutex, spin::SpinNoIrq};

use super::ProcessData;
use crate::mm::AddrSpace;

struct SchedulerAddressSpaceLease {
    aspace: Arc<PiMutex<AddrSpace>>,
    released: AtomicBool,
}

impl Drop for SchedulerAddressSpaceLease {
    fn drop(&mut self) {
        self.release();
    }
}

impl SchedulerAddressSpaceLease {
    fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            crate::mm::release_scheduler_slot(&self.aspace);
        }
    }
}

fn detach_scheduler_address_space(lease: &SchedulerAddressSpaceLease) {
    lease.release();
}

pub(crate) fn scheduler_address_space(
    aspace: Arc<PiMutex<AddrSpace>>,
) -> Result<ax_runtime::task::TaskAddressSpace, ax_runtime::task::TaskError> {
    let (root, active_cpus) = crate::mm::attach_scheduler_slot(&aspace);
    ax_runtime::task::TaskAddressSpace::new_with_task_detach(
        root,
        active_cpus,
        SchedulerAddressSpaceLease {
            aspace,
            released: AtomicBool::new(false),
        },
        detach_scheduler_address_space,
    )
}

/// Address-space state whose release must follow scheduler switch-tail rules.
pub(super) struct ProcessMemoryState {
    aspace: SpinNoIrq<Arc<PiMutex<AddrSpace>>>,
    heap_top: AtomicUsize,
    aspace_slot_released: AtomicBool,
}

impl ProcessMemoryState {
    pub(super) fn new(aspace: Arc<PiMutex<AddrSpace>>) -> Self {
        Self {
            aspace: SpinNoIrq::new(aspace),
            heap_top: AtomicUsize::new(crate::config::USER_HEAP_BASE),
            aspace_slot_released: AtomicBool::new(false),
        }
    }
}

impl ProcessData {
    /// Releases this process's [`AddrSpace::process_slots`] entry once.
    pub fn release_aspace_slot_if_needed(&self) {
        if self
            .memory
            .aspace_slot_released
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let aspace = self.memory.aspace.lock().clone();
        crate::mm::release_process_slot(&aspace);
    }

    /// Returns the top address of the user heap.
    pub fn get_heap_top(&self) -> usize {
        self.memory.heap_top.load(Ordering::Acquire)
    }

    /// Updates the top address of the user heap.
    pub fn set_heap_top(&self, top: usize) {
        self.memory.heap_top.store(top, Ordering::Release)
    }

    /// Returns a strong reference to the current address space.
    pub fn aspace(&self) -> Arc<PiMutex<AddrSpace>> {
        self.memory.aspace.lock().clone()
    }

    /// Creates one owning scheduler token for the current address space.
    pub(crate) fn scheduler_address_space(
        &self,
    ) -> Result<ax_runtime::task::TaskAddressSpace, ax_runtime::task::TaskError> {
        scheduler_address_space(self.aspace())
    }

    /// Publishes a new address space while retaining the replaced one.
    ///
    /// The caller must transfer the new scheduler token and install its active
    /// mm before releasing the returned process slot. Moving the old `Arc` out
    /// of the non-sleeping gate prevents its destructor from acquiring a
    /// sleepable lock while IRQs and preemption are disabled.
    #[must_use = "release the old process slot after committing the new active mm"]
    pub fn stage_aspace_replacement(
        &self,
        new_aspace: Arc<PiMutex<AddrSpace>>,
    ) -> Arc<PiMutex<AddrSpace>> {
        crate::mm::attach_process_slot(&new_aspace);
        {
            let mut guard = self.memory.aspace.lock();
            core::mem::replace(&mut *guard, new_aspace)
        }
    }
}
