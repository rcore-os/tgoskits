use core::{
    cell::UnsafeCell,
    fmt,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
#[cfg(any(test, doctest))]
use std::cell::RefCell;

use ax_kernel_guard::{BaseGuard, IrqSave};

const MAX_LOCKS: usize = 1024;
const MAX_HELD_LOCKS: usize = 32;
const WORDS_PER_ROW: usize = MAX_LOCKS.div_ceil(64);

#[derive(Clone, Copy)]
pub(crate) struct HeldLock {
    pub(crate) id: u32,
    pub(crate) addr: usize,
    pub(crate) caller: &'static Location<'static>,
}

impl fmt::Debug for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldLock")
            .field("id", &self.id)
            .field("addr", &format_args!("{:#x}", self.addr))
            .field("caller", &self.caller)
            .finish()
    }
}

pub(crate) struct HeldLockStack {
    len: usize,
    entries: [Option<HeldLock>; MAX_HELD_LOCKS],
}

impl HeldLockStack {
    pub(crate) const fn new() -> Self {
        Self {
            len: 0,
            entries: [None; MAX_HELD_LOCKS],
        }
    }

    fn iter(&self) -> impl Iterator<Item = HeldLock> + '_ {
        self.entries[..self.len]
            .iter()
            .map(|slot| slot.expect("held lock stack contains empty slot"))
    }

    fn contains(&self, id: u32) -> bool {
        self.iter().any(|held| held.id == id)
    }

    fn push(&mut self, held: HeldLock) {
        assert!(
            self.len < MAX_HELD_LOCKS,
            "lockdep: held lock stack overflow while acquiring {:?}",
            held
        );
        self.entries[self.len] = Some(held);
        self.len += 1;
    }

    fn pop_checked(&mut self, id: u32) {
        assert!(
            self.len != 0,
            "lockdep: releasing lock {id} with empty held lock stack"
        );
        let top = self.entries[self.len - 1]
            .expect("held lock stack top unexpectedly empty during release");
        assert_eq!(
            top.id, id,
            "lockdep: unlock order violation, releasing id={} while top of stack is {:?}",
            id, top
        );
        self.entries[self.len - 1] = None;
        self.len -= 1;
    }
}

impl fmt::Debug for HeldLockStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        for held in self.iter() {
            list.entry(&held);
        }
        list.finish()
    }
}

#[cfg(not(any(test, doctest)))]
#[ax_percpu::def_percpu]
static HELD_LOCKS: HeldLockStack = HeldLockStack::new();

#[cfg(any(test, doctest))]
std::thread_local! {
    static HELD_LOCKS: RefCell<HeldLockStack> = const { RefCell::new(HeldLockStack::new()) };
}

pub(crate) struct LockdepMap {
    id: AtomicU32,
}

impl LockdepMap {
    pub(crate) const fn new() -> Self {
        Self {
            id: AtomicU32::new(0),
        }
    }
}

struct LockGraph {
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

    // SAFETY: Protected by the global graph spinlock above.
    let graph = unsafe { &mut *GRAPH_STATE.graph.get() };
    f(graph)
}

fn with_tracking_context<R>(f: impl FnOnce() -> R) -> R {
    let _guard = IrqSave::new();
    f()
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
        "lockdep: exceeded maximum tracked lock instances ({MAX_LOCKS})"
    );

    map.id.store(new_id, Ordering::Release);
    new_id
}

fn with_held_locks<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
    #[cfg(test)]
    {
        HELD_LOCKS.with(|held_locks| f(&mut held_locks.borrow_mut()))
    }
    #[cfg(doctest)]
    {
        HELD_LOCKS.with(|held_locks| f(&mut held_locks.borrow_mut()))
    }
    #[cfg(not(any(test, doctest)))]
    {
        // SAFETY: tracked guards enter atomic context before lock acquisition.
        f(unsafe { HELD_LOCKS.current_ref_mut_raw() })
    }
}

pub(crate) fn prepare_acquire<G: BaseGuard>(
    map: &LockdepMap,
    addr: usize,
    caller: &'static Location<'static>,
) -> Option<(u32, &'static Location<'static>)> {
    if !G::lockdep_enabled() {
        return None;
    }

    let lock_id = with_tracking_context(|| {
        let lock_id = ensure_lock_id(map);
        with_graph(|graph| {
            with_held_locks(|stack| {
                assert!(
                    !stack.contains(lock_id),
                    "lockdep: recursive spin lock acquisition detected for id={} addr={:#x} at {} \
                     with held stack {:?}",
                    lock_id,
                    addr,
                    caller,
                    stack
                );

                for held in stack.iter() {
                    assert!(
                        !graph.reaches(lock_id, held.id),
                        "lockdep: lock order inversion detected while acquiring id={} addr={:#x} \
                         at {}; held lock {:?}; stack {:?}",
                        lock_id,
                        addr,
                        caller,
                        held,
                        stack
                    );
                }
            });
        });
        lock_id
    });

    Some((lock_id, caller))
}

pub(crate) fn finish_acquire(lockdep: Option<(u32, &'static Location<'static>)>, addr: usize) {
    let Some((lock_id, caller)) = lockdep else {
        return;
    };

    with_tracking_context(|| {
        with_graph(|graph| {
            with_held_locks(|stack| {
                let max_id = NEXT_LOCK_ID.load(Ordering::Acquire).min(MAX_LOCKS as u32);

                for held in stack.iter() {
                    graph.add_order(held.id, lock_id, max_id);
                }

                stack.push(HeldLock {
                    id: lock_id,
                    addr,
                    caller,
                });
            });
        });
    });
}

pub(crate) fn release(lock_id: Option<u32>) {
    let Some(lock_id) = lock_id else {
        return;
    };

    with_tracking_context(|| with_held_locks(|stack| stack.pop_checked(lock_id)));
}

pub(crate) fn force_release<G: BaseGuard>(map: &LockdepMap) {
    if !G::lockdep_enabled() {
        return;
    }

    let lock_id = map.id.load(Ordering::Acquire);
    if lock_id != 0 {
        release(Some(lock_id));
    }
}
