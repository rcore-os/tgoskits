use core::{
    any::type_name,
    cell::UnsafeCell,
    fmt,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
#[cfg(any(test, doctest))]
use std::cell::RefCell;

#[cfg(not(any(test, doctest)))]
use ax_crate_interface::call_interface;
use ax_kernel_guard::{BaseGuard, IrqSave, NoOp};

const MAX_LOCKS: usize = 1024;
const MAX_HELD_LOCKS: usize = 32;
const MAX_HELD_LOCK_SNAPSHOT: usize = MAX_HELD_LOCKS * 2;
const WORDS_PER_ROW: usize = MAX_LOCKS.div_ceil(64);

#[derive(Clone, Copy)]
pub struct HeldLock {
    pub id: u32,
    pub addr: usize,
    pub caller: &'static Location<'static>,
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

#[derive(Clone, Copy)]
pub struct HeldLockStack {
    len: usize,
    entries: [Option<HeldLock>; MAX_HELD_LOCKS],
}

impl HeldLockStack {
    pub const fn new() -> Self {
        Self {
            len: 0,
            entries: [None; MAX_HELD_LOCKS],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = HeldLock> + '_ {
        self.entries[..self.len]
            .iter()
            .map(|slot| slot.expect("held lock stack contains empty slot"))
    }

    pub fn contains(&self, id: u32) -> bool {
        self.iter().any(|held| held.id == id)
    }

    pub fn push(&mut self, held: HeldLock) {
        assert!(
            self.len < MAX_HELD_LOCKS,
            "lockdep: held lock stack overflow while acquiring {:?}",
            held
        );
        self.entries[self.len] = Some(held);
        self.len += 1;
    }

    pub fn pop_checked(&mut self, id: u32) {
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

impl Default for HeldLockStack {
    fn default() -> Self {
        Self::new()
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

#[derive(Clone, Copy)]
pub struct HeldLockSnapshot {
    len: usize,
    entries: [Option<HeldLock>; MAX_HELD_LOCK_SNAPSHOT],
}

impl HeldLockSnapshot {
    pub const fn new() -> Self {
        Self {
            len: 0,
            entries: [None; MAX_HELD_LOCK_SNAPSHOT],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = HeldLock> + '_ {
        self.entries[..self.len]
            .iter()
            .map(|slot| slot.expect("held lock snapshot contains empty slot"))
    }

    pub fn contains(&self, id: u32) -> bool {
        self.iter().any(|held| held.id == id)
    }

    pub fn extend(&mut self, stack: &HeldLockStack) {
        for held in stack.iter() {
            self.push(held);
        }
    }

    pub fn push(&mut self, held: HeldLock) {
        if self.contains(held.id) {
            return;
        }

        assert!(
            self.len < MAX_HELD_LOCK_SNAPSHOT,
            "lockdep: combined held lock snapshot overflow while acquiring {:?}",
            held
        );
        self.entries[self.len] = Some(held);
        self.len += 1;
    }
}

impl Default for HeldLockSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for HeldLockSnapshot {
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

#[cfg(not(any(test, doctest)))]
#[ax_crate_interface::def_interface]
pub trait KspinLockdepIf {
    fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot);
    fn push_current_task_held_lock(held: HeldLock);
    fn pop_current_task_held_lock(lock_id: u32);
}

pub struct LockdepMap {
    id: AtomicU32,
}

impl LockdepMap {
    pub const fn new() -> Self {
        Self {
            id: AtomicU32::new(0),
        }
    }

    pub fn lock_id(&self) -> Option<u32> {
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

pub type LockdepState = (u32, &'static Location<'static>);

#[derive(Clone, Copy)]
enum TrackingTarget {
    Cpu,
    Task,
}

#[derive(Clone, Copy)]
pub struct PreparedAcquire {
    state: LockdepState,
    held_before: HeldLockSnapshot,
    target: TrackingTarget,
}

impl PreparedAcquire {
    pub fn lock_id(self) -> u32 {
        self.state.0
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

    fn assert_can_acquire(
        &self,
        held_locks: &HeldLockSnapshot,
        lock_kind: &str,
        lock_id: u32,
        addr: usize,
        caller: &'static Location<'static>,
    ) {
        assert!(
            !held_locks.contains(lock_id),
            "lockdep: recursive {lock_kind} acquisition detected for id={} addr={:#x} at {} with \
             held stack {:?}",
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
        held_before: &HeldLockSnapshot,
        held_locks: &mut HeldLockStack,
        lock_id: u32,
        addr: usize,
        caller: &'static Location<'static>,
    ) {
        self.record_edges(held_before, lock_id);
        held_locks.push(HeldLock {
            id: lock_id,
            addr,
            caller,
        });
    }

    fn record_edges(&mut self, held_before: &HeldLockSnapshot, lock_id: u32) {
        let max_id = NEXT_LOCK_ID.load(Ordering::Acquire).min(MAX_LOCKS as u32);

        for held in held_before.iter() {
            self.add_order(held.id, lock_id, max_id);
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

fn with_current_cpu_held_locks<R>(f: impl FnOnce(&HeldLockStack) -> R) -> R {
    #[cfg(test)]
    {
        HELD_LOCKS.with(|held_locks| f(&held_locks.borrow()))
    }
    #[cfg(doctest)]
    {
        HELD_LOCKS.with(|held_locks| f(&held_locks.borrow()))
    }
    #[cfg(not(any(test, doctest)))]
    {
        // SAFETY: callers enter tracking context before accessing per-CPU held locks.
        f(unsafe { HELD_LOCKS.current_ref_raw() })
    }
}

fn with_current_cpu_held_locks_mut<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
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
        // SAFETY: callers enter tracking context before mutating per-CPU held locks.
        f(unsafe { HELD_LOCKS.current_ref_mut_raw() })
    }
}

#[cfg(not(any(test, doctest)))]
fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
    call_interface!(KspinLockdepIf::collect_current_task_held_locks, snapshot);
}

#[cfg(any(test, doctest))]
fn collect_current_task_held_locks(_snapshot: &mut HeldLockSnapshot) {}

#[cfg(not(any(test, doctest)))]
fn push_current_task_held_lock(held: HeldLock) {
    call_interface!(KspinLockdepIf::push_current_task_held_lock, held);
}

#[cfg(any(test, doctest))]
fn push_current_task_held_lock(_held: HeldLock) {}

#[cfg(not(any(test, doctest)))]
fn pop_current_task_held_lock(lock_id: u32) {
    call_interface!(KspinLockdepIf::pop_current_task_held_lock, lock_id);
}

#[cfg(any(test, doctest))]
fn pop_current_task_held_lock(_lock_id: u32) {}

fn is_noop_guard<G: BaseGuard>() -> bool {
    type_name::<G>() == type_name::<NoOp>()
}

fn guard_tracks_task_locks<G: BaseGuard>() -> bool {
    is_noop_guard::<G>()
}

fn guard_lockdep_enabled<G: BaseGuard>() -> bool {
    G::lockdep_enabled() || guard_tracks_task_locks::<G>()
}

fn current_held_lock_snapshot() -> HeldLockSnapshot {
    let mut snapshot = current_cpu_held_lock_snapshot();
    collect_current_task_held_locks(&mut snapshot);
    snapshot
}

pub fn current_cpu_held_lock_snapshot() -> HeldLockSnapshot {
    with_tracking_context(|| {
        let mut snapshot = HeldLockSnapshot::new();
        with_current_cpu_held_locks(|stack| snapshot.extend(stack));
        snapshot
    })
}

pub fn prepare_acquire_with_snapshot(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
) -> PreparedAcquire {
    prepare_acquire_with_target_snapshot(
        map,
        lock_kind,
        addr,
        caller,
        held_before,
        TrackingTarget::Task,
    )
}

fn prepare_acquire_with_target_snapshot(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    target: TrackingTarget,
) -> PreparedAcquire {
    let lock_id = ensure_lock_id(map);
    with_graph(|graph| {
        graph.assert_can_acquire(&held_before, lock_kind, lock_id, addr, caller);
    });
    PreparedAcquire {
        state: (lock_id, caller),
        held_before,
        target,
    }
}

pub fn prepare_acquire<G: BaseGuard>(
    map: &LockdepMap,
    addr: usize,
    caller: &'static Location<'static>,
) -> Option<PreparedAcquire> {
    if !guard_lockdep_enabled::<G>() {
        return None;
    }

    let held_before = with_tracking_context(current_held_lock_snapshot);
    Some(prepare_acquire_with_target_snapshot(
        map,
        "spin lock",
        addr,
        caller,
        held_before,
        if guard_tracks_task_locks::<G>() {
            TrackingTarget::Task
        } else {
            TrackingTarget::Cpu
        },
    ))
}

pub fn finish_acquire_with_stack(
    prepared: PreparedAcquire,
    addr: usize,
    held_locks: &mut HeldLockStack,
) {
    let (lock_id, caller) = prepared.state;
    with_graph(|graph| {
        graph.record_acquire(&prepared.held_before, held_locks, lock_id, addr, caller)
    });
}

pub fn finish_acquire(prepared: Option<PreparedAcquire>, addr: usize) {
    let Some(prepared) = prepared else {
        return;
    };

    match prepared.target {
        TrackingTarget::Cpu => with_tracking_context(|| {
            with_current_cpu_held_locks_mut(|stack| {
                finish_acquire_with_stack(prepared, addr, stack)
            });
        }),
        TrackingTarget::Task => {
            let (lock_id, caller) = prepared.state;
            with_graph(|graph| graph.record_edges(&prepared.held_before, lock_id));
            push_current_task_held_lock(HeldLock {
                id: lock_id,
                addr,
                caller,
            });
        }
    }
}

pub fn release_from_stack(lock_id: Option<u32>, held_locks: &mut HeldLockStack) {
    if let Some(lock_id) = lock_id {
        held_locks.pop_checked(lock_id);
    }
}

pub fn release<G: BaseGuard>(lock_id: Option<u32>) {
    if guard_tracks_task_locks::<G>() {
        if let Some(lock_id) = lock_id {
            pop_current_task_held_lock(lock_id);
        }
        return;
    }

    with_tracking_context(|| {
        with_current_cpu_held_locks_mut(|stack| release_from_stack(lock_id, stack))
    });
}

pub fn force_release<G: BaseGuard>(map: &LockdepMap) {
    if !guard_lockdep_enabled::<G>() {
        return;
    }

    let Some(lock_id) = map.lock_id() else {
        return;
    };

    if guard_tracks_task_locks::<G>() {
        pop_current_task_held_lock(lock_id);
        return;
    }

    with_tracking_context(|| {
        with_current_cpu_held_locks_mut(|stack| release_from_stack(Some(lock_id), stack))
    });
}
