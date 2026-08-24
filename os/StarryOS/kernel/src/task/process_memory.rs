//! Process address-space ownership and exit-time release.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use super::ProcessData;
use crate::{
    mm::AddrSpace,
    sync::{IrqMutex, PiMutex, PreemptGuard},
    task::futex::FutexDomain,
};

/// One Linux mm generation and every facility whose identity follows it.
///
/// `CLONE_VM` shares this object even when it creates a distinct process.
/// `fork` and `exec` create a new object. Keeping the private futex domain next
/// to the address space prevents process/thread-group identity from becoming a
/// second, conflicting definition of private-futex ownership.
struct ProcessMemoryOwner {
    aspace: Arc<PiMutex<AddrSpace>>,
    private_futexes: Arc<FutexDomain>,
}

/// Rare-writer publication cell for one process mm generation.
struct ProcessMemoryOwnerCell<T> {
    current: AtomicPtr<T>,
    reader_epoch: AtomicUsize,
    readers: [AtomicUsize; 2],
    writer: IrqMutex<()>,
    #[cfg(axtest)]
    locked_snapshots: AtomicUsize,
}

impl<T> ProcessMemoryOwnerCell<T> {
    fn new(current: Arc<T>) -> Self {
        Self {
            current: AtomicPtr::new(Arc::into_raw(current).cast_mut()),
            reader_epoch: AtomicUsize::new(0),
            readers: [AtomicUsize::new(0), AtomicUsize::new(0)],
            writer: IrqMutex::new(()),
            #[cfg(axtest)]
            locked_snapshots: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> Arc<T> {
        self.snapshot_after_load(|| {})
    }

    fn replace(&self, next: Arc<T>) -> Arc<T> {
        self.replace_after_publish(next, || {})
    }

    fn snapshot_after_load(&self, after_load: impl FnOnce()) -> Arc<T> {
        // A replacing exec may run on this CPU after a task-context reader is
        // preempted. Pin the short raw-pointer acquisition so the writer can
        // never wait for a reader which it prevented from resuming.
        let _reader_pin = PreemptGuard::new();
        loop {
            let epoch = self.reader_epoch.load(Ordering::Acquire);
            debug_assert!(epoch < self.readers.len());
            self.readers[epoch].fetch_add(1, Ordering::AcqRel);
            if self.reader_epoch.load(Ordering::Acquire) != epoch {
                self.readers[epoch].fetch_sub(1, Ordering::Release);
                continue;
            }

            let current = self.current.load(Ordering::Acquire);
            debug_assert!(!current.is_null());
            after_load();
            // SAFETY: this reader joined `epoch` before loading `current`.
            // Replacement publishes the next pointer, advances the epoch, and
            // waits for every reader from the old epoch before releasing the
            // publication's strong reference. The pointee therefore remains
            // live until this independent strong reference is acquired.
            let snapshot = unsafe {
                Arc::increment_strong_count(current);
                Arc::from_raw(current)
            };
            self.readers[epoch].fetch_sub(1, Ordering::Release);
            return snapshot;
        }
    }

    fn replace_after_publish(&self, next: Arc<T>, after_publish: impl FnOnce()) -> Arc<T> {
        let writer = self.writer.lock();
        let next = Arc::into_raw(next).cast_mut();
        let previous = self.current.swap(next, Ordering::AcqRel);
        let previous_epoch = self.reader_epoch.fetch_xor(1, Ordering::AcqRel);
        debug_assert!(previous_epoch < self.readers.len());
        after_publish();
        while self.readers[previous_epoch].load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        // SAFETY: `previous` was created by `Arc::into_raw` and the old reader
        // epoch is now quiescent. Returning the reconstructed strong reference
        // also keeps its destructor outside the non-sleeping writer gate.
        let previous = unsafe { Arc::from_raw(previous) };
        drop(writer);
        previous
    }

    #[cfg(axtest)]
    fn locked_snapshot_count(&self) -> usize {
        self.locked_snapshots.load(Ordering::Relaxed)
    }
}

impl<T> Drop for ProcessMemoryOwnerCell<T> {
    fn drop(&mut self) {
        debug_assert_eq!(*self.readers[0].get_mut(), 0);
        debug_assert_eq!(*self.readers[1].get_mut(), 0);
        let current = *self.current.get_mut();
        debug_assert!(!current.is_null());
        // SAFETY: mutable access proves no snapshot or replacement can be in
        // flight. `current` still owns the strong reference installed by
        // `new` or the last `replace`.
        unsafe { drop(Arc::from_raw(current)) };
    }
}

impl ProcessMemoryOwner {
    fn new(aspace: Arc<PiMutex<AddrSpace>>) -> Self {
        Self {
            aspace,
            private_futexes: Arc::new(FutexDomain::new_private()),
        }
    }
}

/// Opaque strong reference used to share or retire one mm generation.
#[derive(Clone)]
pub(crate) struct ProcessMemoryShare(Arc<ProcessMemoryOwner>);

impl ProcessMemoryShare {
    pub(crate) fn aspace(&self) -> Arc<PiMutex<AddrSpace>> {
        self.0.aspace.clone()
    }

    pub(crate) fn private_futexes(&self) -> Arc<FutexDomain> {
        self.0.private_futexes.clone()
    }
}

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
    owner: ProcessMemoryOwnerCell<ProcessMemoryOwner>,
    heap_top: AtomicUsize,
    aspace_slot_released: AtomicBool,
}

impl ProcessMemoryState {
    pub(super) fn new(aspace: Arc<PiMutex<AddrSpace>>, shared: Option<ProcessMemoryShare>) -> Self {
        Self {
            owner: ProcessMemoryOwnerCell::new(shared.map_or_else(
                || Arc::new(ProcessMemoryOwner::new(aspace)),
                |share| share.0,
            )),
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
        let aspace = self.memory.owner.snapshot().aspace.clone();
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
        self.memory.owner.snapshot().aspace.clone()
    }

    /// Captures the current mm generation once for clone/futex/teardown.
    pub(crate) fn memory_share(&self) -> ProcessMemoryShare {
        ProcessMemoryShare(self.memory.owner.snapshot())
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
    pub fn stage_memory_replacement(
        &self,
        new_aspace: Arc<PiMutex<AddrSpace>>,
    ) -> ProcessMemoryShare {
        crate::mm::attach_process_slot(&new_aspace);
        let new_owner = Arc::new(ProcessMemoryOwner::new(new_aspace));
        ProcessMemoryShare(self.memory.owner.replace(new_owner))
    }
}

#[cfg(axtest)]
pub(crate) fn memory_owner_snapshot_avoids_irq_lock_for_test() -> bool {
    let owner = ProcessMemoryOwnerCell::new(Arc::new(7usize));
    let snapshot = owner.snapshot();
    *snapshot == 7 && owner.locked_snapshot_count() == 0
}

#[cfg(axtest)]
pub(crate) fn memory_owner_replacement_preserves_pinned_snapshot_for_test() -> bool {
    let Ok(cpu_count) = ax_runtime::task::cpu_topology_len() else {
        return false;
    };
    if cpu_count < 2 {
        return false;
    }
    let mut reader_affinity = ax_runtime::task::CpuSet::empty(cpu_count);
    reader_affinity.insert(ax_runtime::task::CpuId::new(1));
    let mut writer_affinity = ax_runtime::task::CpuSet::empty(cpu_count);
    writer_affinity.insert(ax_runtime::task::CpuId::new(0));

    let owner = Arc::new(ProcessMemoryOwnerCell::new(Arc::new(7usize)));
    let reader_loaded = Arc::new(AtomicBool::new(false));
    let writer_published = Arc::new(AtomicBool::new(false));
    let reader_value = Arc::new(AtomicUsize::new(0));
    let previous_value = Arc::new(AtomicUsize::new(0));

    let reader = {
        let owner = owner.clone();
        let reader_loaded = reader_loaded.clone();
        let writer_published = writer_published.clone();
        let reader_value = reader_value.clone();
        ax_std::thread::spawn(move || {
            ax_runtime::task::set_current_thread_affinity(reader_affinity)
                .expect("the snapshot reader must be pinned to its test CPU");
            let snapshot = owner.snapshot_after_load(|| {
                reader_loaded.store(true, Ordering::Release);
                while !writer_published.load(Ordering::Acquire) {
                    core::hint::spin_loop();
                }
            });
            reader_value.store(*snapshot, Ordering::Release);
        })
    };
    while !reader_loaded.load(Ordering::Acquire) {
        ax_std::thread::yield_now();
    }

    let writer = {
        let owner = owner.clone();
        let writer_published = writer_published.clone();
        let previous_value = previous_value.clone();
        ax_std::thread::spawn(move || {
            ax_runtime::task::set_current_thread_affinity(writer_affinity)
                .expect("the replacement writer must be pinned to its test CPU");
            let previous = owner.replace_after_publish(Arc::new(9), || {
                writer_published.store(true, Ordering::Release);
            });
            previous_value.store(*previous, Ordering::Release);
        })
    };
    while !writer_published.load(Ordering::Acquire) {
        ax_std::thread::yield_now();
    }

    reader.join().unwrap();
    writer.join().unwrap();
    let published = owner.snapshot();

    *published == 9
        && reader_value.load(Ordering::Acquire) == 7
        && previous_value.load(Ordering::Acquire) == 7
        && *owner.snapshot() == 9
}
