//! Host-only `ax_sync` lock provider for the `axdevice` test binaries.
//!
//! Production links the native scheduler provider: `MutexOps` is a
//! scheduler-aware PI mutex, while `ContextOps`/`SpinOps` own real IRQ and
//! preemption state. A plain host test binary has no scheduled thread and no
//! hardware IRQ source, so this module links a genuine blocking
//! `std::sync::Mutex` + `Condvar` `MutexOps` and formal primitive-boundary
//! `ContextOps`/`SpinOps` helpers.
//!
//! The host provider proves borrow, locking and blocking contracts only: it
//! models no hardware IRQ, no preemption and no PI donate/runqueue behaviour.
//! Those remain a root/board QEMU validation concern. The production mutex
//! backend is never replaced by a spin loop here.

extern crate std;

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Condvar, Mutex, OnceLock},
};

use ax_sync::interface::{AcquireResult, ContextState, LockMetadata, MutexStorage};

/// One host sleepable mutex: a std blocking mutex plus a condvar wait queue.
struct HostSleepMutex {
    held: Mutex<bool>,
    released: Condvar,
}

impl HostSleepMutex {
    fn new() -> Self {
        Self {
            held: Mutex::new(false),
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

static COMPANIONS: OnceLock<Mutex<BTreeMap<usize, Arc<HostSleepMutex>>>> = OnceLock::new();

fn companions() -> &'static Mutex<BTreeMap<usize, Arc<HostSleepMutex>>> {
    COMPANIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn companion(lock_addr: usize) -> Arc<HostSleepMutex> {
    let mut table = companions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    table
        .entry(lock_addr)
        .or_insert_with(|| Arc::new(HostSleepMutex::new()))
        .clone()
}

fn existing_companion(lock_addr: usize) -> Option<Arc<HostSleepMutex>> {
    companions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&lock_addr)
        .cloned()
}

thread_local! {
    static OWNED: RefCell<BTreeSet<usize>> = const { RefCell::new(BTreeSet::new()) };
}

fn set_owned(lock_addr: usize, owned: bool) {
    OWNED.with(|slot| {
        if owned {
            slot.borrow_mut().insert(lock_addr);
        } else {
            slot.borrow_mut().remove(&lock_addr);
        }
    });
}

fn release_companion(storage: &MutexStorage, lock_addr: usize) {
    let key = storage.owner_word().load(Ordering::Acquire) as usize;
    let target = if key == 0 { lock_addr } else { key };
    if let Some(companion) = existing_companion(target) {
        companion.unlock();
    }
    storage.wait_state().store(0, Ordering::Release);
    set_owned(target, false);
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
        // The owner word persists the companion key so ownership queries and
        // destruction find this mutex without trusting backend object layout.
        storage
            .owner_word()
            .store(lock_addr as u64, Ordering::Release);
        companion(lock_addr).lock();
        // `wait_state` is the host locked flag; the native PI waiter tree never
        // runs in host tests, so this byte is otherwise unused here.
        storage.wait_state().store(1, Ordering::Release);
        set_owned(lock_addr, true);
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
        if !companion(lock_addr).try_lock() {
            return false;
        }
        storage.wait_state().store(1, Ordering::Release);
        set_owned(lock_addr, true);
        true
    }

    fn release(storage: &MutexStorage, lock_addr: usize) {
        release_companion(storage, lock_addr);
    }

    fn force_release(storage: &MutexStorage, lock_addr: usize) {
        release_companion(storage, lock_addr);
    }

    fn is_owned_by_current(storage: &MutexStorage) -> bool {
        let key = storage.owner_word().load(Ordering::Acquire) as usize;
        key != 0 && OWNED.with(|slot| slot.borrow().contains(&key))
    }

    fn is_locked(storage: &MutexStorage) -> bool {
        storage.wait_state().load(Ordering::Acquire) != 0
    }

    fn destroy(storage: &mut MutexStorage) {
        let key = storage.owner_word().load(Ordering::Acquire) as usize;
        if key != 0 {
            // The wrapper asserts the mutex is unlocked before destroying it,
            // so the companion is dropped outside the registry guard.
            let removed = companions()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&key);
            drop(removed);
            storage.owner_word().store(0, Ordering::Release);
        }
        storage.wait_state().store(0, Ordering::Release);
    }
}

struct HostSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for HostSpinOps {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> ContextState {
        while locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        ContextState::new(0, 0)
    }

    fn try_acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _context: u8,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        AcquireResult::new(
            locked
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok(),
            ContextState::new(0, 0),
        )
    }

    fn release(locked: &AtomicBool, _lock_addr: usize, _context: u8, _state: ContextState) {
        locked.store(false, Ordering::Release);
    }

    fn force_release(locked: &AtomicBool, _lock_addr: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Acquire)
    }
}

struct HostContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for HostContextOps {
    fn enter(_context: u8) -> ContextState {
        // No hardware IRQ source or preemption state exists in host tests; the
        // formal boundary accepts and restores an empty token.
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
