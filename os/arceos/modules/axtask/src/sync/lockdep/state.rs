//! Lock classes, dependency graph, and acquire/release orchestration.

use core::{
    cell::UnsafeCell,
    panic::Location,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
};

use super::{
    backend::{
        collect_current_task_held_locks, lockdep_fatal, pop_current_task_held_lock,
        push_current_task_held_lock,
    },
    types::*,
};

#[repr(C)]
pub struct LockdepMap {
    class_id: AtomicU32,
    class_key: AtomicPtr<Location<'static>>,
}

impl LockdepMap {
    #[track_caller]
    pub const fn new() -> Self {
        Self::new_with_class_key(Location::caller() as *const Location<'static>)
    }

    pub const fn new_dynamic() -> Self {
        Self::new_with_class_key(ptr::null())
    }

    const fn new_with_class_key(class_key: *const Location<'static>) -> Self {
        Self {
            class_id: AtomicU32::new(0),
            class_key: AtomicPtr::new(class_key as *mut Location<'static>),
        }
    }

    pub fn class_id(&self) -> Option<u32> {
        match self.class_id.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }

    /// Borrows lock-class storage supplied by the `ax-sync` bridge.
    ///
    /// # Safety
    ///
    /// `class_id` and `class_key` must be the first two fields of one live
    /// `#[repr(C)]` object with the same layout as `LockdepMap`. The object must
    /// outlive the returned borrow and must not be moved while it is borrowed.
    #[doc(hidden)]
    pub unsafe fn from_external_parts<'a>(
        class_id: &'a AtomicU32,
        class_key: &'a AtomicPtr<Location<'static>>,
    ) -> &'a Self {
        // SAFETY: guaranteed by the caller's shared-layout contract.
        let map = unsafe { &*(core::ptr::from_ref(class_id).cast::<Self>()) };
        debug_assert_eq!(
            core::ptr::from_ref(&map.class_key),
            core::ptr::from_ref(class_key)
        );
        map
    }
}

impl Default for LockdepMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub struct PreparedAcquire {
    state: LockdepState,
    held_before: HeldLockSnapshot,
    kind: HeldLockKind,
    mode: HeldLockMode,
    edge_mode: AcquireEdgeMode,
    sleep_forbidden: bool,
}

impl PreparedAcquire {
    pub fn class_id(self) -> u32 {
        self.state.class_id
    }
}

#[derive(Clone, Copy)]
struct LockdepState {
    class_id: u32,
    caller: &'static Location<'static>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LockdepCheckError {
    Recursive,
    OrderInversion,
}

#[derive(Clone, Copy)]
enum AcquireEdgeMode {
    Record,
    NestedRead,
}

struct ClassRegistry {
    keys: [usize; MAX_LOCK_CLASSES],
}

impl ClassRegistry {
    const fn new() -> Self {
        Self {
            keys: [0; MAX_LOCK_CLASSES],
        }
    }

    fn find_or_register(&mut self, key: usize) -> u32 {
        let max_id = NEXT_CLASS_ID
            .load(Ordering::Acquire)
            .min(MAX_LOCK_CLASSES as u32);

        for class_id in 1..max_id {
            if self.keys[class_id as usize] == key {
                return class_id;
            }
        }

        let new_id = NEXT_CLASS_ID.fetch_add(1, Ordering::AcqRel);
        if (new_id as usize) >= MAX_LOCK_CLASSES {
            lockdep_fatal(format_args!(
                "lockdep: exceeded maximum tracked lock classes ({MAX_LOCK_CLASSES})"
            ));
        }

        self.keys[new_id as usize] = key;
        new_id
    }

    fn subclass(&self, class_id: u32) -> LockSubclass {
        self.keys
            .get(class_id as usize)
            .copied()
            .filter(|key| *key != 0)
            .map(class_key_subclass)
            .unwrap_or(DEFAULT_LOCK_SUBCLASS)
    }
}

struct LockGraph {
    reachability: [[u64; WORDS_PER_ROW]; MAX_LOCK_CLASSES],
}

impl LockGraph {
    const fn new() -> Self {
        Self {
            reachability: [[0; WORDS_PER_ROW]; MAX_LOCK_CLASSES],
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
        if before as usize >= MAX_LOCK_CLASSES || after as usize >= MAX_LOCK_CLASSES {
            lockdep_fatal(format_args!(
                "lockdep: invalid class edge {} -> {} exceeds maximum tracked lock classes ({})",
                before, after, MAX_LOCK_CLASSES
            ));
        }
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

    fn check_can_acquire(
        &self,
        held_locks: &HeldLockSnapshot,
        addr: usize,
        class_id: u32,
        mode: HeldLockMode,
    ) -> Result<AcquireEdgeMode, LockdepCheckError> {
        if let Some(held) = held_locks.iter().find(|held| held.addr == addr) {
            if held.mode.allows_same_lock_nesting(mode) {
                return Ok(AcquireEdgeMode::NestedRead);
            }
            return Err(LockdepCheckError::Recursive);
        }

        for held in held_locks.iter() {
            if self.reaches(class_id, held.class_id) {
                return Err(LockdepCheckError::OrderInversion);
            }
        }
        Ok(AcquireEdgeMode::Record)
    }

    fn record_edges(&mut self, held_before: &HeldLockSnapshot, class_id: u32) {
        let max_id = NEXT_CLASS_ID
            .load(Ordering::Acquire)
            .min(MAX_LOCK_CLASSES as u32);

        for held in held_before.iter() {
            self.add_order(held.class_id, class_id, max_id);
        }
    }

    #[cfg(test)]
    fn record_acquire(
        &mut self,
        held_before: &HeldLockSnapshot,
        held_locks: &mut HeldLockStack,
        prepared: PreparedAcquire,
        addr: usize,
    ) {
        if matches!(prepared.edge_mode, AcquireEdgeMode::Record) {
            self.record_edges(held_before, prepared.state.class_id);
        }
        held_locks.push(HeldLock {
            class_id: prepared.state.class_id,
            kind: prepared.kind,
            mode: prepared.mode,
            sleep_forbidden: prepared.sleep_forbidden,
            addr,
            caller: prepared.state.caller,
        });
    }
}

struct GraphState {
    lock: AtomicBool,
    graph: UnsafeCell<LockGraph>,
    classes: UnsafeCell<ClassRegistry>,
}

unsafe impl Sync for GraphState {}

struct GraphGuard {
    #[cfg(not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))))]
    irq_state: usize,
}

impl GraphGuard {
    fn acquire() -> Self {
        #[cfg(not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))))]
        let irq_state = crate::sync::irq_save_and_disable();
        while GRAPH_STATE
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while GRAPH_STATE.lock.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        Self {
            #[cfg(not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))))]
            irq_state,
        }
    }
}

impl Drop for GraphGuard {
    fn drop(&mut self) {
        GRAPH_STATE.lock.store(false, Ordering::Release);
        #[cfg(not(any(test, doctest, all(feature = "host-test", not(target_os = "none")))))]
        // SAFETY: this is the matching, properly nested restore for the state
        // saved when this graph guard was acquired.
        unsafe {
            crate::sync::irq_restore(self.irq_state)
        };
    }
}

static NEXT_CLASS_ID: AtomicU32 = AtomicU32::new(1);

static GRAPH_STATE: GraphState = GraphState {
    lock: AtomicBool::new(false),
    graph: UnsafeCell::new(LockGraph::new()),
    classes: UnsafeCell::new(ClassRegistry::new()),
};

fn with_graph<R>(f: impl FnOnce(&mut LockGraph) -> R) -> R {
    let _guard = GraphGuard::acquire();

    // SAFETY: Protected by the global graph spinlock above.
    let graph = unsafe { &mut *GRAPH_STATE.graph.get() };
    f(graph)
}

fn ensure_class(
    map: &LockdepMap,
    class_key: *const Location<'static>,
    subclass: LockSubclass,
) -> LockdepState {
    let existing_class = map.class_id.load(Ordering::Acquire);
    if subclass == DEFAULT_LOCK_SUBCLASS && existing_class != 0 {
        return LockdepState {
            class_id: existing_class,
            caller: class_key_to_location(class_key),
        };
    }

    let _guard = GraphGuard::acquire();

    let key = match map.class_key.load(Ordering::Acquire) {
        ptr if ptr.is_null() => {
            map.class_key
                .store(class_key as *mut Location<'static>, Ordering::Release);
            class_key
        }
        ptr => ptr as *const Location<'static>,
    };

    let default_class_id = match map.class_id.load(Ordering::Acquire) {
        0 => {
            // SAFETY: protected by the global graph spinlock above.
            let classes = unsafe { &mut *GRAPH_STATE.classes.get() };
            let id = classes.find_or_register(pack_class_key(key, DEFAULT_LOCK_SUBCLASS));
            map.class_id.store(id, Ordering::Release);
            id
        }
        id => id,
    };

    let class_id = if subclass == DEFAULT_LOCK_SUBCLASS {
        default_class_id
    } else {
        // SAFETY: protected by the global graph spinlock above.
        let classes = unsafe { &mut *GRAPH_STATE.classes.get() };
        classes.find_or_register(pack_class_key(key, subclass))
    };

    LockdepState {
        class_id,
        caller: class_key_to_location(class_key),
    }
}

fn pack_class_key(class_key: *const Location<'static>, subclass: LockSubclass) -> usize {
    let key = class_key as usize;
    let subclass = subclass as usize;
    if subclass > LOCK_SUBCLASS_MASK {
        lockdep_fatal(format_args!(
            "lockdep: subclass {subclass} exceeds maximum {}",
            LOCK_SUBCLASS_MASK
        ));
    }
    if key & LOCK_SUBCLASS_MASK != 0 {
        lockdep_fatal(format_args!(
            "lockdep: class key {key:#x} is not aligned enough to encode subclasses"
        ));
    }
    key | subclass
}

fn class_key_subclass(key: usize) -> LockSubclass {
    (key & LOCK_SUBCLASS_MASK) as LockSubclass
}

fn class_subclass(class_id: u32) -> LockSubclass {
    let _guard = GraphGuard::acquire();
    // SAFETY: protected by the global graph spinlock above.
    let classes = unsafe { &*GRAPH_STATE.classes.get() };
    classes.subclass(class_id)
}

fn held_lock_subclass_snapshot(snapshot: &HeldLockSnapshot) -> HeldLockSubclassSnapshot {
    let _guard = GraphGuard::acquire();
    // SAFETY: protected by the global graph spinlock above.
    let classes = unsafe { &*GRAPH_STATE.classes.get() };
    let mut values = [DEFAULT_LOCK_SUBCLASS; MAX_HELD_LOCK_SNAPSHOT];
    for (index, held) in snapshot.iter().enumerate() {
        values[index] = classes.subclass(held.class_id);
    }
    HeldLockSubclassSnapshot { values }
}

fn class_key_to_location(class_key: *const Location<'static>) -> &'static Location<'static> {
    // SAFETY: class keys are constructed from `Location::caller()` references.
    unsafe { &*class_key }
}

pub fn current_task_held_lock_snapshot() -> HeldLockSnapshot {
    let mut snapshot = HeldLockSnapshot::new();
    collect_current_task_held_locks(&mut snapshot);
    snapshot
}

#[derive(Clone, Copy)]
struct AcquireRequest {
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    subclass: LockSubclass,
    sleep_forbidden: bool,
    mode: HeldLockMode,
}

pub(crate) fn prepare_acquire_with_snapshot_nested_mode(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
    mode: HeldLockMode,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_nested_with_sleep_and_mode(
        map,
        held_before,
        AcquireRequest {
            lock_kind,
            addr,
            caller,
            subclass,
            sleep_forbidden: true,
            mode,
        },
    )
}

pub(crate) fn prepare_acquire_with_snapshot_nested_with_sleep(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
    sleep_forbidden: bool,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_nested_with_sleep_and_mode(
        map,
        held_before,
        AcquireRequest {
            lock_kind,
            addr,
            caller,
            subclass,
            sleep_forbidden,
            mode: HeldLockMode::Exclusive,
        },
    )
}

fn prepare_acquire_with_snapshot_nested_with_sleep_and_mode(
    map: &LockdepMap,
    held_before: HeldLockSnapshot,
    request: AcquireRequest,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_result(map, held_before, request).unwrap_or_else(
        |(err, state)| {
            fatal_on_lockdep_error(err, request.lock_kind, state, request.addr, &held_before)
        },
    )
}

#[cfg(test)]
fn prepare_acquire_with_snapshot_checked(
    map: &LockdepMap,
    _lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
) -> Result<PreparedAcquire, LockdepCheckError> {
    prepare_acquire_with_snapshot_checked_nested(
        map,
        _lock_kind,
        addr,
        caller,
        held_before,
        DEFAULT_LOCK_SUBCLASS,
    )
}

#[cfg(test)]
fn prepare_acquire_with_snapshot_checked_nested(
    map: &LockdepMap,
    _lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
) -> Result<PreparedAcquire, LockdepCheckError> {
    prepare_acquire_with_snapshot_result(
        map,
        held_before,
        AcquireRequest {
            lock_kind: _lock_kind,
            addr,
            caller,
            subclass,
            sleep_forbidden: true,
            mode: HeldLockMode::Exclusive,
        },
    )
    .map_err(|(err, _state)| err)
}

fn prepare_acquire_with_snapshot_result(
    map: &LockdepMap,
    held_before: HeldLockSnapshot,
    request: AcquireRequest,
) -> Result<PreparedAcquire, (LockdepCheckError, LockdepState)> {
    let class_key = request.caller as *const Location<'static>;
    let state = ensure_class(map, class_key, request.subclass);
    let edge_mode = with_graph(|graph| {
        graph.check_can_acquire(&held_before, request.addr, state.class_id, request.mode)
    })
    .map_err(|err| (err, state))?;
    Ok(PreparedAcquire {
        state,
        held_before,
        kind: HeldLockKind::from_label(request.lock_kind),
        mode: request.mode,
        edge_mode,
        sleep_forbidden: request.sleep_forbidden,
    })
}

fn fatal_on_lockdep_error(
    err: LockdepCheckError,
    lock_kind: &str,
    state: LockdepState,
    addr: usize,
    held_before: &HeldLockSnapshot,
) -> ! {
    let requested_class = state.class_id;
    let requested_subclass = class_subclass(requested_class);
    let held_subclasses = held_lock_subclass_snapshot(held_before);
    match err {
        LockdepCheckError::Recursive => {
            let (held_index, held) = conflicting_held_lock(
                held_before,
                |held| held.addr == addr,
                "lockdep: recursive acquire without held lock snapshot",
            );
            lockdep_fatal(format_args!(
                "lockdep: recursive {lock_kind} acquisition detected\nrequested:\n  class={} \
                 subclass={} addr={:#x} acquire_at={}\nalready held:\n  {}\nheld stack:\n{}",
                requested_class,
                requested_subclass,
                addr,
                state.caller,
                HeldLockDisplay {
                    held: &held,
                    subclass: held_subclasses.get(held_index),
                },
                HeldLockStackDisplay {
                    snapshot: held_before,
                    subclasses: &held_subclasses,
                }
            ))
        }
        LockdepCheckError::OrderInversion => {
            let (held_index, held) = conflicting_held_lock(
                held_before,
                |held| with_graph(|graph| graph.reaches(requested_class, held.class_id)),
                "lockdep: order inversion without held lock snapshot",
            );
            lockdep_fatal(format_args!(
                "lockdep: lock order inversion detected\nrequested:\n  kind={} class={} \
                 subclass={} addr={:#x} acquire_at={}\nconflicting held lock:\n  {}\nheld \
                 stack:\n{}",
                lock_kind,
                requested_class,
                requested_subclass,
                addr,
                state.caller,
                HeldLockDisplay {
                    held: &held,
                    subclass: held_subclasses.get(held_index),
                },
                HeldLockStackDisplay {
                    snapshot: held_before,
                    subclasses: &held_subclasses,
                }
            ));
        }
    }
}

fn conflicting_held_lock(
    held_before: &HeldLockSnapshot,
    matches: impl Fn(HeldLock) -> bool,
    empty_message: &'static str,
) -> (usize, HeldLock) {
    for (index, held) in held_before.iter().enumerate() {
        if matches(held) {
            return (index, held);
        }
    }

    if let Some((index, held)) = held_before.iter().enumerate().next() {
        return (index, held);
    }

    lockdep_fatal(format_args!("{empty_message}"))
}

#[cfg(test)]
fn finish_acquire_with_stack(
    prepared: PreparedAcquire,
    addr: usize,
    held_locks: &mut HeldLockStack,
) {
    with_graph(|graph| graph.record_acquire(&prepared.held_before, held_locks, prepared, addr));
}

pub(crate) fn finish_acquire_task(prepared: PreparedAcquire, addr: usize) {
    if matches!(prepared.edge_mode, AcquireEdgeMode::Record) {
        with_graph(|graph| graph.record_edges(&prepared.held_before, prepared.state.class_id));
    }
    push_current_task_held_lock(HeldLock {
        class_id: prepared.state.class_id,
        kind: prepared.kind,
        mode: prepared.mode,
        sleep_forbidden: prepared.sleep_forbidden,
        addr,
        caller: prepared.state.caller,
    });
}

#[cfg(test)]
fn release_from_stack(lock_addr: usize, held_locks: &mut HeldLockStack) {
    held_locks.pop_checked(lock_addr);
}

pub(crate) fn release_task(lock_addr: usize) {
    pop_current_task_held_lock(lock_addr);
}

pub(crate) fn force_release_task(lock_addr: usize) {
    release_task(lock_addr);
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
