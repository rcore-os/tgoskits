//! Host-only `ax_sync` lock provider for the `axvirtio-net` test binaries.
//!
//! A single fixture file serves every `axvirtio-net` test surface, so the
//! `MutexOps`/`ContextOps`/`SpinOps` implementations exist exactly once per
//! test binary:
//!
//! * the crate's own unit tests include it through
//!   `#[cfg(test)] #[path = "../tests/common/mod.rs"] mod host_lock_provider;`
//! * the integration test includes it through `mod common;`
//!
//! Production links the native scheduler provider instead: `MutexOps` is a
//! scheduler-aware PI mutex, and `ContextOps`/`SpinOps` own real IRQ and
//! preemption state. A host test binary has no scheduled task and no hardware
//! IRQ source, so this fixture links a genuine blocking `std::sync::Mutex` +
//! `Condvar` `MutexOps` plus formal primitive-boundary `ContextOps`/`SpinOps`
//! helpers. It proves borrow, locking and blocking contracts only: it models no
//! hardware IRQ, no preemption and no PI donate/runqueue behaviour, which stay a
//! root/board QEMU concern. No production code path depends on this fixture and
//! the production mutex backend is never replaced by a spin loop here.
//!
//! # Companion lifetime
//!
//! Each `ax_sync::Mutex` gets one `HostSleepMutex` companion, addressed by its
//! mutex storage address. The companion is inserted into `COMPANIONS` on the
//! first `acquire` (or `try_acquire` probe) and removed by `MutexOps::destroy`,
//! which the `ax_sync` `Mutex` wrapper calls exactly once from its `Drop`, with
//! no live guard and no waiter. Removing it on `unlock` instead would strand a
//! waiter already queued on the companion condvar, so `destroy` is the only
//! race-free cleanup point. The table is therefore bounded by the number of
//! live mutexes; a mutex that is never dropped (a `static` or a deliberately
//! leaked instance) keeps its single entry for the process lifetime.

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
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        while *held {
            held = self.released.wait(held).unwrap_or_else(|p| p.into_inner());
        }
        *held = true;
    }

    fn try_lock(&self) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        if *held {
            return false;
        }
        *held = true;
        true
    }

    fn unlock(&self) {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        *held = false;
        drop(held);
        self.released.notify_one();
    }
}

static COMPANIONS: OnceLock<Mutex<BTreeMap<usize, Arc<HostSleepMutex>>>> = OnceLock::new();

fn companions() -> &'static Mutex<BTreeMap<usize, Arc<HostSleepMutex>>> {
    COMPANIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn companion(addr: usize) -> Arc<HostSleepMutex> {
    companions()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(addr)
        .or_insert_with(|| Arc::new(HostSleepMutex::new()))
        .clone()
}

fn existing_companion(addr: usize) -> Option<Arc<HostSleepMutex>> {
    companions()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&addr)
        .cloned()
}

std::thread_local! {
    static OWNED: RefCell<BTreeSet<usize>> = const { RefCell::new(BTreeSet::new()) };
}

fn set_owned(addr: usize, owned: bool) {
    OWNED.with(|slot| {
        if owned {
            slot.borrow_mut().insert(addr);
        } else {
            slot.borrow_mut().remove(&addr);
        }
    });
}

fn release_companion(storage: &MutexStorage, addr: usize) {
    let key = storage.owner_word().load(Ordering::Acquire) as usize;
    let target = if key == 0 { addr } else { key };
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
        storage
            .owner_word()
            .store(lock_addr as u64, Ordering::Release);
        companion(lock_addr).lock();
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
            let removed = companions()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
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
