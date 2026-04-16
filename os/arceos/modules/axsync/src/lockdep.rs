use core::{
    cell::UnsafeCell,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use ax_task::HeldLock;

use crate::mutex::RawMutex;

const MAX_LOCKS: usize = 1024;
const WORDS_PER_ROW: usize = MAX_LOCKS.div_ceil(64);
type LockdepState = (u32, &'static Location<'static>);

pub(crate) struct LockdepMap {
    id: AtomicU32,
}

impl LockdepMap {
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicU32::new(0),
        }
    }

    pub(crate) fn lock_id(&self) -> Option<u32> {
        match self.id.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }
}

impl Default for LockdepMap {
    fn default() -> Self {
        Self::new()
    }
}

struct LockGraph {
    // Reachability encodes the observed "A before B" mutex ordering graph.
    reachability: [[u64; WORDS_PER_ROW]; MAX_LOCKS],
}

impl LockGraph {
    const fn new() -> Self {
        Self {
            reachability: [[0; WORDS_PER_ROW]; MAX_LOCKS],
        }
    }

    fn reaches(&self, from: u32, to: u32) -> bool {
        let Some(row) = self.reachability.get(from as usize) else {
            return false;
        };
        let word = (to as usize) / 64;
        let bit = (to as usize) % 64;
        row.get(word)
            .is_some_and(|entry| (*entry & (1u64 << bit)) != 0)
    }

    fn add_order(&mut self, before: u32, after: u32, max_id: u32) {
        let mut closure = self.reachability[after as usize];
        let word = (after as usize) / 64;
        let bit = (after as usize) % 64;
        closure[word] |= 1u64 << bit;

        for row in 1..max_id {
            if row == before || self.reaches(row, before) {
                for (slot, extra) in self.reachability[row as usize].iter_mut().zip(closure) {
                    *slot |= extra;
                }
            }
        }
    }

    fn assert_can_acquire(
        &self,
        held_locks: &ax_task::HeldLockStack,
        lock_id: u32,
        addr: usize,
        caller: &'static Location<'static>,
    ) {
        // Sleeping locks are tracked per task, not per CPU: the owner may be
        // rescheduled or migrated while still holding the mutex.
        assert!(
            !held_locks.contains(lock_id),
            "lockdep: recursive mutex acquisition detected for id={} addr={:#x} at {} with held \
             stack {:?}",
            lock_id,
            addr,
            caller,
            held_locks
        );

        for held in held_locks.iter() {
            assert!(
                !self.reaches(lock_id, held.id),
                "lockdep: lock order inversion detected while acquiring id={} addr={:#x} at {}; \
                 held lock {:?}; stack {:?}",
                lock_id,
                addr,
                caller,
                held,
                held_locks
            );
        }
    }

    fn record_acquire(
        &mut self,
        held_locks: &mut ax_task::HeldLockStack,
        lock_id: u32,
        addr: usize,
        caller: &'static Location<'static>,
        max_id: u32,
    ) {
        // Snapshot the currently held mutexes before pushing the new one so we
        // record edges from the previous lock set to this acquisition.
        let snapshot = *held_locks;
        for held in snapshot.iter() {
            self.add_order(held.id, lock_id, max_id);
        }

        held_locks.push(HeldLock {
            id: lock_id,
            addr,
            caller,
        });
    }
}

struct GraphState {
    lock: AtomicBool,
    graph: UnsafeCell<LockGraph>,
}

unsafe impl Sync for GraphState {}

struct GraphGuard;

impl GraphGuard {
    fn acquire() -> Self {
        while GRAPH_STATE
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while GRAPH_STATE.lock.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        Self
    }
}

impl Drop for GraphGuard {
    fn drop(&mut self) {
        GRAPH_STATE.lock.store(false, Ordering::Release);
    }
}

static NEXT_LOCK_ID: AtomicU32 = AtomicU32::new(1);

static GRAPH_STATE: GraphState = GraphState {
    lock: AtomicBool::new(false),
    graph: UnsafeCell::new(LockGraph::new()),
};

fn with_graph<R>(f: impl FnOnce(&mut LockGraph) -> R) -> R {
    let _guard = GraphGuard::acquire();

    // SAFETY: protected by the global graph spinlock above.
    let graph = unsafe { &mut *GRAPH_STATE.graph.get() };
    f(graph)
}

fn current_max_lock_id() -> u32 {
    NEXT_LOCK_ID.load(Ordering::Acquire).min(MAX_LOCKS as u32)
}

fn ensure_lock_id(map: &LockdepMap) -> u32 {
    let existing = map.id.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }

    let _guard = GraphGuard::acquire();

    let existing = map.id.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }

    let new_id = NEXT_LOCK_ID.fetch_add(1, Ordering::AcqRel);
    assert!(
        (new_id as usize) < MAX_LOCKS,
        "lockdep: exceeded maximum tracked mutex instances ({MAX_LOCKS})"
    );

    map.id.store(new_id, Ordering::Release);
    new_id
}

fn prepare_acquire(
    map: &LockdepMap,
    addr: usize,
    caller: &'static Location<'static>,
) -> LockdepState {
    let lock_id = ensure_lock_id(map);

    with_graph(|graph| {
        // Validate against the task-local held-lock stack before the mutex is
        // actually acquired so failed acquisitions do not mutate lockdep state.
        ax_task::with_current_lockdep_stack(|stack| {
            graph.assert_can_acquire(stack, lock_id, addr, caller)
        });
    });

    (lock_id, caller)
}

fn finish_acquire(lockdep: LockdepState, addr: usize) {
    let (lock_id, caller) = lockdep;
    let max_id = current_max_lock_id();

    with_graph(|graph| {
        ax_task::with_current_lockdep_stack(|stack| {
            graph.record_acquire(stack, lock_id, addr, caller, max_id);
        });
    });
}

pub(crate) struct LockdepAcquire {
    addr: usize,
    state: LockdepState,
}

impl LockdepAcquire {
    #[inline(always)]
    #[track_caller]
    pub(crate) fn prepare(lock: &RawMutex) -> Self {
        let addr = lock as *const _ as *const () as usize;
        let state = prepare_acquire(&lock.lockdep, addr, Location::caller());
        Self { addr, state }
    }

    #[inline(always)]
    pub(crate) fn finish(self) {
        finish_acquire(self.state, self.addr);
    }
}

#[inline(always)]
pub(crate) fn release(lock: &RawMutex) {
    let Some(lock_id) = lock.lockdep.lock_id() else {
        return;
    };

    // RawMutex is non-recursive and ownership is task-based, so release is a
    // simple pop from the current task's held-lock stack.
    ax_task::with_current_lockdep_stack(|stack| stack.pop_checked(lock_id));
}
