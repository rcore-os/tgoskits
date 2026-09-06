//! Process address-space ownership and exit-time release.

use alloc::sync::Arc;
#[cfg(axtest)]
use core::sync::atomic::{AtomicBool, AtomicUsize};
use core::sync::atomic::{AtomicPtr, Ordering};

mod reader_epoch;
use reader_epoch::ReaderEpoch;

use super::ProcessData;
use crate::{
    mm::{MmHandle, MmPin, TransparentHugePageMode},
    sync::{IrqMutex, PreemptGuard},
    task::futex::FutexDomain,
};

/// One Linux mm generation and every facility whose identity follows it.
///
/// Each process owns one MmHandle. `CLONE_VM` shares its MM and private futex
/// domain through explicit user-reference cloning; `fork` and `exec` replace both. Keeping the private futex domain next
/// to the address space prevents process/thread-group identity from becoming a
/// second, conflicting definition of private-futex ownership.
struct ProcessMemoryOwner {
    mm: MmHandle,
    private_futexes: Arc<FutexDomain>,
}

/// Rare-writer publication cell for one process mm generation.
struct ProcessMemoryOwnerCell<T> {
    current: AtomicPtr<T>,
    readers: ReaderEpoch,
    writer: IrqMutex<()>,
    #[cfg(axtest)]
    locked_snapshots: AtomicUsize,
}

impl<T> ProcessMemoryOwnerCell<T> {
    fn new(current: Arc<T>) -> Self {
        Self {
            current: AtomicPtr::new(Arc::into_raw(current).cast_mut()),
            readers: ReaderEpoch::new(),
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
            let Some(epoch) = self.readers.enter() else {
                continue;
            };

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
            self.readers.leave(epoch);
            return snapshot;
        }
    }

    fn replace_after_publish(&self, next: Arc<T>, after_publish: impl FnOnce()) -> Arc<T> {
        let writer = self.writer.lock();
        let next = Arc::into_raw(next).cast_mut();
        let previous = self.current.swap(next, Ordering::AcqRel);
        let previous_epoch = self.readers.advance();
        after_publish();
        while !self.readers.is_quiescent(previous_epoch) {
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
        debug_assert!(self.readers.is_quiescent(0));
        debug_assert!(self.readers.is_quiescent(1));
        let current = *self.current.get_mut();
        debug_assert!(!current.is_null());
        // SAFETY: mutable access proves no snapshot or replacement can be in
        // flight. `current` still owns the strong reference installed by
        // `new` or the last `replace`.
        unsafe { drop(Arc::from_raw(current)) };
    }
}

impl ProcessMemoryOwner {
    fn new(mm: MmHandle, shared: Option<ProcessMemoryShare>) -> Self {
        let private_futexes = shared.map_or_else(
            || Arc::new(FutexDomain::new_private()),
            |shared| {
                assert_eq!(mm.id(), shared.0.mm.id());
                shared.private_futexes()
            },
        );
        Self {
            mm,
            private_futexes,
        }
    }
}

/// Snapshot of a process's MM generation; it does not create a process owner.
#[derive(Clone)]
pub(crate) struct ProcessMemoryShare(Arc<ProcessMemoryOwner>);

impl ProcessMemoryShare {
    pub(crate) fn aspace(&self) -> MmPin {
        self.0
            .mm
            .pin()
            .expect("a live syscall must retain a live MM")
    }

    pub(crate) fn private_futexes(&self) -> Arc<FutexDomain> {
        self.0.private_futexes.clone()
    }

    pub(crate) fn private_futexes_ref(&self) -> &Arc<FutexDomain> {
        &self.0.private_futexes
    }

    pub(crate) fn retire(&self) {
        if let Some(permit) = self.0.mm.release_user_ref() {
            crate::mm::enqueue_retire(permit);
        }
    }
}

pub(crate) fn scheduler_address_space(
    mm: &MmHandle,
) -> Result<ax_runtime::task::TaskAddressSpace, ax_runtime::task::TaskError> {
    mm.scheduler_address_space()
}

/// Unpublished MM owner prepared before exec starts sibling teardown.
/// Dropping it before publication retires the unused image normally.
pub(crate) struct PreparedProcessMemory(Arc<ProcessMemoryOwner>);

impl PreparedProcessMemory {
    pub(crate) fn new(mm: MmHandle) -> Self {
        Self(Arc::new(ProcessMemoryOwner::new(mm, None)))
    }
}

/// Process publication owns one MmHandle; CPU and kernel ownership live in MM.
pub(super) struct ProcessMemoryState {
    owner: ProcessMemoryOwnerCell<ProcessMemoryOwner>,
}

impl ProcessMemoryState {
    pub(super) fn new(mm: MmHandle, shared: Option<ProcessMemoryShare>) -> Self {
        Self {
            owner: ProcessMemoryOwnerCell::new(Arc::new(ProcessMemoryOwner::new(mm, shared))),
        }
    }
}

impl ProcessData {
    /// Ends process ownership; pins and CPU activations defer actual reclaim.
    pub fn retire_mm_owner(&self) {
        self.memory_share().retire();
    }

    pub fn clone_aspace_user_ref(&self) -> Result<MmHandle, crate::mm::CloneUserRefError> {
        self.memory.owner.snapshot().mm.clone_user_ref()
    }

    pub fn pin_aspace(&self) -> crate::StarryResult<MmPin> {
        self.memory
            .owner
            .snapshot()
            .mm
            .pin()
            .map_err(|_| crate::StarryError::BadState)
    }

    /// Pins the address space for a kernel operation on a live process.
    pub fn aspace(&self) -> MmPin {
        self.pin_aspace()
            .expect("operation requires a live process MM")
    }

    pub fn transparent_huge_page_mode(&self) -> TransparentHugePageMode {
        self.memory.owner.snapshot().mm.transparent_huge_page_mode()
    }

    pub fn set_transparent_huge_page_mode(
        &self,
        mode: TransparentHugePageMode,
    ) -> crate::StarryResult<()> {
        self.pin_aspace()?.set_transparent_huge_page_mode(mode);
        Ok(())
    }

    /// Captures the current mm generation once for clone/futex/teardown.
    pub(crate) fn memory_share(&self) -> ProcessMemoryShare {
        ProcessMemoryShare(self.memory.owner.snapshot())
    }

    pub(crate) fn scheduler_address_space(
        &self,
    ) -> Result<ax_runtime::task::TaskAddressSpace, ax_runtime::task::TaskError> {
        self.memory.owner.snapshot().mm.scheduler_address_space()
    }

    /// Publishes the new MM and returns the old owner for retirement after the
    /// hardware switch. All allocation happens before entering the writer gate.
    #[must_use = "retire the old MM only after committing the new active MM"]
    pub(crate) fn stage_memory_replacement(
        &self,
        prepared: PreparedProcessMemory,
    ) -> ProcessMemoryShare {
        ProcessMemoryShare(self.memory.owner.replace(prepared.0))
    }
}

#[cfg(axtest)]
fn memory_owner_snapshot_avoids_irq_lock_for_test() -> bool {
    let owner = ProcessMemoryOwnerCell::new(Arc::new(7usize));
    let snapshot = owner.snapshot();
    *snapshot == 7 && owner.locked_snapshot_count() == 0
}

#[cfg(axtest)]
fn memory_owner_replacement_preserves_pinned_snapshot_for_test() -> bool {
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

#[cfg(axtest)]
fn thread_page_table_lease_follows_task_lifetime_for_test() -> bool {
    let Ok(mut aspace) = crate::mm::new_user_aspace_empty() else {
        return false;
    };
    if crate::mm::copy_from_kernel(&mut aspace).is_err() {
        return false;
    }

    let mm = MmHandle::from_arc(Arc::new(crate::sync::PiMutex::new(aspace))).unwrap();
    let task_aspace = match scheduler_address_space(&mm) {
        Ok(task_aspace) => task_aspace,
        Err(_) => return false,
    };
    let before_detach = mm.kernel_pins() == 1;
    let no_early_retirement = mm.release_user_ref().is_none();
    let retiring = mm.state() == crate::mm::MmState::Retiring;
    drop(task_aspace);
    before_detach && no_early_retirement && retiring && mm.kernel_pins() == 0
}

#[cfg(all(test, axtest))]
mod axtests {
    #[axtest::axtest]
    fn memory_owner_snapshot_avoids_irq_lock() {
        assert!(super::memory_owner_snapshot_avoids_irq_lock_for_test());
    }

    #[axtest::axtest]
    fn memory_owner_replacement_preserves_pinned_snapshot() {
        assert!(super::memory_owner_replacement_preserves_pinned_snapshot_for_test());
    }

    #[axtest::axtest]
    fn thread_page_table_lease_follows_task_lifetime() {
        assert!(super::thread_page_table_lease_follows_task_lifetime_for_test());
    }
}
