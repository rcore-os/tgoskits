//! Blocking mutex adapter for host component tests.
//!
//! The portable lock calls its normal MutexOps boundary. A host process uses
//! real std blocking and has no scheduler-owned PI state. Native kernel builds
//! select RuntimeMutexOps; this adapter does not model task scheduling or PI.

use core::{
    panic::Location,
    sync::atomic::{AtomicU64, Ordering},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Condvar, Mutex, OnceLock},
};

use ax_sync::interface::{LockMetadata, MutexStorage};

struct BlockingMutex {
    held: Mutex<bool>,
    available: Condvar,
}

impl BlockingMutex {
    fn new() -> Self {
        Self {
            held: Mutex::new(false),
            available: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        while *held {
            held = self
                .available
                .wait(held)
                .unwrap_or_else(|error| error.into_inner());
        }
        *held = true;
    }

    fn try_acquire(&self) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        if *held {
            return false;
        }
        *held = true;
        true
    }

    fn release(&self) {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        assert!(*held, "host mutex release requires an owner");
        *held = false;
        drop(held);
        self.available.notify_one();
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static MUTEXES: OnceLock<Mutex<BTreeMap<u64, Arc<BlockingMutex>>>> = OnceLock::new();

std::thread_local! {
    static OWNED: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
}

fn mutexes() -> &'static Mutex<BTreeMap<u64, Arc<BlockingMutex>>> {
    MUTEXES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn companion(storage: &MutexStorage) -> (u64, Arc<BlockingMutex>) {
    let mut table = mutexes().lock().unwrap_or_else(|error| error.into_inner());
    let mut id = storage.generation().load(Ordering::Acquire);
    if id == 0 {
        id = NEXT_ID
            .try_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .expect("host mutex identity space exhausted");
        storage.generation().store(id, Ordering::Release);
    }
    let companion = table
        .entry(id)
        .or_insert_with(|| Arc::new(BlockingMutex::new()))
        .clone();
    (id, companion)
}

fn acquired(storage: &MutexStorage, id: u64) {
    OWNED.with(|owned| {
        assert!(
            owned.borrow_mut().insert(id),
            "host mutex recursive acquisition"
        )
    });
    storage.owner_word().store(id, Ordering::Release);
    storage.wait_state().store(1, Ordering::Release);
}

fn release(storage: &MutexStorage) {
    let id = storage.generation().load(Ordering::Acquire);
    let companion = mutexes()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&id)
        .cloned()
        .expect("host mutex release requires a live companion");
    OWNED.with(|owned| {
        assert!(
            owned.borrow_mut().remove(&id),
            "host mutex released by a non-owner"
        )
    });
    storage.owner_word().store(0, Ordering::Release);
    storage.wait_state().store(0, Ordering::Release);
    companion.release();
}

struct HostMutexOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for HostMutexOps {
    fn acquire(
        storage: &MutexStorage,
        _next_waiter_sequence: &AtomicU64,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) {
        let (id, companion) = companion(storage);
        OWNED.with(|owned| {
            assert!(
                !owned.borrow().contains(&id),
                "host mutex recursive acquisition"
            )
        });
        companion.acquire();
        acquired(storage, id);
    }

    fn try_acquire(
        storage: &MutexStorage,
        _next_waiter_sequence: &AtomicU64,
        _metadata: &LockMetadata,
        _lock_addr: usize,
        _subclass: u32,
        _caller: &'static Location<'static>,
    ) -> bool {
        let (id, companion) = companion(storage);
        if !companion.try_acquire() {
            return false;
        }
        acquired(storage, id);
        true
    }

    fn release(storage: &MutexStorage, _lock_addr: usize) {
        release(storage);
    }
    fn force_release(storage: &MutexStorage, _lock_addr: usize) {
        release(storage);
    }
    fn is_owned_by_current(storage: &MutexStorage) -> bool {
        let id = storage.generation().load(Ordering::Acquire);
        id != 0 && OWNED.with(|owned| owned.borrow().contains(&id))
    }
    fn is_locked(storage: &MutexStorage) -> bool {
        storage.wait_state().load(Ordering::Acquire) != 0
    }
    fn destroy(storage: &mut MutexStorage) {
        let id = storage.generation().load(Ordering::Acquire);
        let removed = mutexes()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
        drop(removed);
        storage.generation().store(0, Ordering::Release);
    }
}
