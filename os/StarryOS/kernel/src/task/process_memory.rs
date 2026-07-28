//! Process address-space ownership and exit-time release.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_sync::{PiMutex, spin::SpinNoIrq};

use super::ProcessData;
use crate::mm::AddrSpace;

/// Address-space state whose release must follow scheduler switch-tail rules.
pub(super) struct ProcessMemoryState {
    aspace: SpinNoIrq<Arc<PiMutex<AddrSpace>>>,
    heap_top: AtomicUsize,
    vm_aspace_shared: AtomicBool,
    aspace_slot_released: AtomicBool,
}

impl ProcessMemoryState {
    pub(super) fn new(aspace: Arc<PiMutex<AddrSpace>>, vm_aspace_shared: bool) -> Self {
        Self {
            aspace: SpinNoIrq::new(aspace),
            heap_top: AtomicUsize::new(crate::config::USER_HEAP_BASE),
            vm_aspace_shared: AtomicBool::new(vm_aspace_shared),
            aspace_slot_released: AtomicBool::new(false),
        }
    }
}

impl ProcessData {
    /// Marks the freshly installed exec address space as private.
    #[inline]
    pub fn mark_vm_aspace_private_after_exec(&self) {
        self.memory.vm_aspace_shared.store(false, Ordering::Release);
    }

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

    /// Publishes a new address space while retaining the replaced one.
    ///
    /// The caller must install the new hardware page-table root before
    /// releasing the returned address-space slot. Moving the old `Arc` out of
    /// the non-sleeping gate prevents its destructor from acquiring a sleepable
    /// lock while IRQs and preemption are disabled.
    #[must_use = "the old address space must be released after installing the new page-table root"]
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
