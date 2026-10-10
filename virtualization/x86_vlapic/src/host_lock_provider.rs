//! Host-test `ax_sync` lock provider for this crate's own unit tests.
//!
//! Production links the x86 vLAPIC against the native ArceOS scheduler
//! provider through `ax_sync`'s portable `ContextOps`/`SpinOps`/`MutexOps`
//! interfaces. Host unit tests cannot use that provider, so this fixture
//! implements the same external interfaces: short IRQ-safe spin state with host
//! atomics and the sleepable mutex with real `std` blocking + condvar.
//!
//! It validates only the non-hardware part of the contract: mutual exclusion,
//! the acquisition context the production lock requests, balanced guard
//! release, and blocking mutual exclusion for the sleepable mutex. Physical
//! local-IRQ save/restore is a native-runtime property and is deliberately not
//! claimed here.

extern crate std;

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Condvar, Mutex as StdMutex, OnceLock},
};

use ax_sync::interface::{AcquireResult, ContextState, LockMetadata, MutexStorage};

/// Sentinel recorded before the first acquisition on a host test thread.
pub(crate) const NO_ACQUIRE: u8 = u8::MAX;

std::thread_local! {
    static LAST_ACQUIRE: Cell<u8> = const { Cell::new(NO_ACQUIRE) };
    static LAST_ACQUIRE_STATE: Cell<Option<ContextState>> = const { Cell::new(None) };
    static LAST_RELEASE_STATE: Cell<Option<ContextState>> = const { Cell::new(None) };
    static ACQUIRES: Cell<usize> = const { Cell::new(0) };
    static RELEASES: Cell<usize> = const { Cell::new(0) };
}

/// Acquisition context requested by the most recent acquisition on this thread.
pub(crate) fn last_acquire_context() -> u8 {
    LAST_ACQUIRE.with(|slot| slot.get())
}

/// Opaque context the most recent acquisition on this thread returned, which a
/// matching guard release must hand back unchanged.
pub(crate) fn last_acquire_state() -> Option<ContextState> {
    LAST_ACQUIRE_STATE.with(|slot| slot.get())
}

/// Context the most recent release on this thread received from its guard.
pub(crate) fn last_release_state() -> Option<ContextState> {
    LAST_RELEASE_STATE.with(|slot| slot.get())
}

/// Number of spin acquisitions observed on this thread.
pub(crate) fn acquire_count() -> usize {
    ACQUIRES.with(|slot| slot.get())
}

/// Number of spin releases observed on this thread.
pub(crate) fn release_count() -> usize {
    RELEASES.with(|slot| slot.get())
}

struct TestContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for TestContextOps {
    fn enter(_context: u8) -> ContextState {
        ContextState::new(0, 0)
    }

    fn exit(_context: u8, _state: ContextState) {}

    fn irq_return_preempt_enter() -> usize {
        0
    }

    fn irq_return_preempt_exit(_state: usize) {}

    fn hardirq_enter() {}

    fn hardirq_exit() {}
}

struct TestSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for TestSpinOps {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        LAST_ACQUIRE.with(|slot| slot.set(context));
        // A distinctive opaque token lets the caller prove the guard restores
        // exactly the state its acquisition returned.
        let state = ContextState::new(0x5a5a, 0xa5a5);
        LAST_ACQUIRE_STATE.with(|slot| slot.set(Some(state)));
        while locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        ACQUIRES.with(|slot| slot.set(slot.get() + 1));
        state
    }

    fn try_acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        let acquired = locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        if acquired {
            LAST_ACQUIRE.with(|slot| slot.set(context));
            LAST_ACQUIRE_STATE.with(|slot| slot.set(Some(ContextState::new(0x5a5a, 0xa5a5))));
            ACQUIRES.with(|slot| slot.set(slot.get() + 1));
        }
        AcquireResult::new(acquired, ContextState::new(0x5a5a, 0xa5a5))
    }

    fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, state: ContextState) {
        LAST_RELEASE_STATE.with(|slot| slot.set(Some(state)));
        locked.store(false, Ordering::Release);
        RELEASES.with(|slot| slot.set(slot.get() + 1));
    }

    fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Acquire)
    }
}

/// One host sleepable mutex: a `std` blocking mutex plus a condvar wait queue.
///
/// It backs the production `ax_sync::Mutex` contract with genuine host blocking
/// so the crate's unit tests can construct and drop the sleepable PIT timer.
/// It is still only a host fixture: the native scheduler-owned PI mutex remains
/// the production provider.
struct HostSleepMutex {
    held: StdMutex<bool>,
    released: Condvar,
}

impl HostSleepMutex {
    fn new() -> Self {
        Self {
            held: StdMutex::new(false),
            released: Condvar::new(),
        }
    }

    fn lock(&self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *held {
            held = self
                .released
                .wait(held)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *held = true;
    }

    fn try_lock(&self) -> bool {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *held {
            false
        } else {
            *held = true;
            true
        }
    }

    fn unlock(&self) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *held = false;
        drop(held);
        self.released.notify_one();
    }
}

static MUTEX_COMPANIONS: OnceLock<StdMutex<BTreeMap<usize, Arc<HostSleepMutex>>>> = OnceLock::new();

fn mutex_companions() -> &'static StdMutex<BTreeMap<usize, Arc<HostSleepMutex>>> {
    MUTEX_COMPANIONS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

fn mutex_companion(lock_addr: usize) -> Arc<HostSleepMutex> {
    let mut table = mutex_companions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table
        .entry(lock_addr)
        .or_insert_with(|| Arc::new(HostSleepMutex::new()))
        .clone()
}

std::thread_local! {
    static OWNED_MUTEXES: RefCell<BTreeSet<usize>> = const { RefCell::new(BTreeSet::new()) };
}

fn set_mutex_owned(lock_addr: usize, owned: bool) {
    OWNED_MUTEXES.with(|slot| {
        if owned {
            slot.borrow_mut().insert(lock_addr);
        } else {
            slot.borrow_mut().remove(&lock_addr);
        }
    });
}

fn release_host_mutex(storage: &MutexStorage, lock_addr: usize) {
    let key = storage.owner_word().load(Ordering::Acquire) as usize;
    let target = if key == 0 { lock_addr } else { key };
    if let Some(companion) = mutex_companions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&target)
        .cloned()
    {
        companion.unlock();
    }
    storage.wait_state().store(0, Ordering::Release);
    set_mutex_owned(target, false);
}

struct HostMutexOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for HostMutexOps {
    fn acquire(
        storage: &MutexStorage,
        _next_waiter_sequence: &AtomicU64,
        _metadata: &LockMetadata,
        lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) {
        storage
            .owner_word()
            .store(lock_addr as u64, Ordering::Release);
        mutex_companion(lock_addr).lock();
        storage.wait_state().store(1, Ordering::Release);
        set_mutex_owned(lock_addr, true);
    }

    fn try_acquire(
        storage: &MutexStorage,
        _next_waiter_sequence: &AtomicU64,
        _metadata: &LockMetadata,
        lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> bool {
        storage
            .owner_word()
            .store(lock_addr as u64, Ordering::Release);
        if !mutex_companion(lock_addr).try_lock() {
            return false;
        }
        storage.wait_state().store(1, Ordering::Release);
        set_mutex_owned(lock_addr, true);
        true
    }

    fn release(storage: &MutexStorage, lock_addr: usize) {
        release_host_mutex(storage, lock_addr);
    }

    fn force_release(storage: &MutexStorage, lock_addr: usize) {
        release_host_mutex(storage, lock_addr);
    }

    fn is_owned_by_current(storage: &MutexStorage) -> bool {
        let key = storage.owner_word().load(Ordering::Acquire) as usize;
        key != 0 && OWNED_MUTEXES.with(|slot| slot.borrow().contains(&key))
    }

    fn is_locked(storage: &MutexStorage) -> bool {
        storage.wait_state().load(Ordering::Acquire) != 0
    }

    fn destroy(storage: &mut MutexStorage) {
        let key = storage.owner_word().load(Ordering::Acquire) as usize;
        if key != 0 {
            let removed = mutex_companions()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&key);
            drop(removed);
            storage.owner_word().store(0, Ordering::Release);
        }
        storage.wait_state().store(0, Ordering::Release);
    }
}
