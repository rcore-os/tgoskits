use core::{
    cell::UnsafeCell,
    fmt,
    panic::Location,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
};
#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
use std::cell::RefCell;

#[cfg(feature = "task-context")]
use ax_crate_interface::call_interface;

const MAX_LOCKS: usize = 1024;
const MAX_HELD_LOCKS: usize = 32;
const MAX_HELD_LOCK_SNAPSHOT: usize = MAX_HELD_LOCKS;
const WORDS_PER_ROW: usize = MAX_LOCKS.div_ceil(64);

#[derive(Clone, Copy)]
pub struct HeldLock {
    pub id: u32,
    pub class_id: u32,
    pub addr: usize,
    pub caller: &'static Location<'static>,
}

impl HeldLock {
    #[track_caller]
    const fn placeholder() -> Self {
        Self {
            id: 0,
            class_id: 0,
            addr: 0,
            caller: Location::caller(),
        }
    }
}

impl fmt::Debug for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldLock")
            .field("id", &self.id)
            .field("class_id", &self.class_id)
            .field("addr", &format_args!("{:#x}", self.addr))
            .field("caller", &self.caller)
            .finish()
    }
}

impl fmt::Display for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "id={} class={} addr={:#x} acquired_at={}",
            self.id, self.class_id, self.addr, self.caller
        )
    }
}

#[derive(Clone, Copy)]
pub struct HeldLockStack {
    depth: usize,
    entries: [HeldLock; MAX_HELD_LOCKS],
}

impl HeldLockStack {
    pub const fn new() -> Self {
        Self {
            depth: 0,
            entries: [HeldLock::placeholder(); MAX_HELD_LOCKS],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = HeldLock> + '_ {
        self.entries[..self.depth].iter().copied()
    }

    // The live held-lock stack must preserve exact acquisition state; callers
    // use this for checks, but push/pop must not silently deduplicate entries.
    pub fn contains(&self, id: u32) -> bool {
        self.iter().any(|held| held.id == id)
    }

    pub fn push(&mut self, held: HeldLock) {
        assert!(
            !self.contains(held.id),
            "lockdep: duplicate held lock push while acquiring {:?}; stack {:?}",
            held,
            self
        );
        assert!(
            self.depth < MAX_HELD_LOCKS,
            "lockdep: held lock stack overflow while acquiring {:?}",
            held
        );
        self.entries[self.depth] = held;
        self.depth += 1;
    }

    pub fn pop_checked(&mut self, id: u32) {
        assert!(
            self.depth != 0,
            "lockdep: releasing lock {id} with empty held lock stack"
        );
        let top = self.entries[self.depth - 1];
        assert_eq!(
            top.id, id,
            "lockdep: unlock order violation, releasing id={} while top of stack is {:?}",
            id, top
        );
        self.depth -= 1;
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
    depth: usize,
    entries: [HeldLock; MAX_HELD_LOCK_SNAPSHOT],
}

impl HeldLockSnapshot {
    pub const fn new() -> Self {
        Self {
            depth: 0,
            entries: [HeldLock::placeholder(); MAX_HELD_LOCK_SNAPSHOT],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = HeldLock> + '_ {
        self.entries[..self.depth].iter().copied()
    }

    // A snapshot is a temporary set-like view used for acquire checks, so
    // duplicate lock ids are filtered out when extending/pushing into it.
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
            self.depth < MAX_HELD_LOCK_SNAPSHOT,
            "lockdep: combined held lock snapshot overflow while acquiring {:?}",
            held
        );
        self.entries[self.depth] = held;
        self.depth += 1;
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

struct HeldLockStackDisplay<'a>(&'a HeldLockSnapshot);

impl fmt::Display for HeldLockStackDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.depth == 0 {
            return write!(f, "  (empty)");
        }

        for (index, held) in self.0.iter().enumerate() {
            let relation = if index + 1 == self.0.depth {
                "top"
            } else {
                "held"
            };
            writeln!(f, "  [{}] {}: {}", index, relation, held)?;
        }
        Ok(())
    }
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
std::thread_local! {
    static HELD_LOCKS: RefCell<HeldLockStack> = const { RefCell::new(HeldLockStack::new()) };
}

#[ax_crate_interface::def_interface]
pub trait KspinLockdepIf {
    fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot);
    fn push_current_task_held_lock(held: HeldLock);
    fn pop_current_task_held_lock(lock_id: u32);
    fn console_write_str(s: &str);
}

pub struct LockdepMap {
    instance_id: AtomicU32,
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
            instance_id: AtomicU32::new(0),
            class_id: AtomicU32::new(0),
            class_key: AtomicPtr::new(class_key as *mut Location<'static>),
        }
    }

    pub fn lock_id(&self) -> Option<u32> {
        match self.instance_id.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }

    pub fn class_id(&self) -> Option<u32> {
        match self.class_id.load(Ordering::Acquire) {
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

#[derive(Clone, Copy)]
pub struct PreparedAcquire {
    state: LockdepState,
    held_before: HeldLockSnapshot,
}

impl PreparedAcquire {
    pub fn lock_id(self) -> u32 {
        self.state.instance_id
    }

    pub fn class_id(self) -> u32 {
        self.state.class_id
    }
}

#[derive(Clone, Copy)]
struct LockdepState {
    instance_id: u32,
    class_id: u32,
    caller: &'static Location<'static>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockdepCheckError {
    Recursive,
    OrderInversion,
}

struct ClassRegistry {
    keys: [usize; MAX_LOCKS],
}

impl ClassRegistry {
    const fn new() -> Self {
        Self {
            keys: [0; MAX_LOCKS],
        }
    }

    fn find_or_register(&mut self, key: usize) -> u32 {
        let max_id = NEXT_CLASS_ID.load(Ordering::Acquire).min(MAX_LOCKS as u32);

        for class_id in 1..max_id {
            if self.keys[class_id as usize] == key {
                return class_id;
            }
        }

        let new_id = NEXT_CLASS_ID.fetch_add(1, Ordering::AcqRel);
        assert!(
            (new_id as usize) < MAX_LOCKS,
            "lockdep: exceeded maximum tracked lock classes ({MAX_LOCKS})"
        );

        self.keys[new_id as usize] = key;
        new_id
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

    fn check_can_acquire(
        &self,
        held_locks: &HeldLockSnapshot,
        instance_id: u32,
        class_id: u32,
    ) -> Result<(), LockdepCheckError> {
        if held_locks.contains(instance_id) {
            return Err(LockdepCheckError::Recursive);
        }

        for held in held_locks.iter() {
            if self.reaches(class_id, held.class_id) {
                return Err(LockdepCheckError::OrderInversion);
            }
        }
        Ok(())
    }

    fn record_edges(&mut self, held_before: &HeldLockSnapshot, class_id: u32) {
        let max_id = NEXT_CLASS_ID.load(Ordering::Acquire).min(MAX_LOCKS as u32);

        for held in held_before.iter() {
            self.add_order(held.class_id, class_id, max_id);
        }
    }

    fn record_acquire(
        &mut self,
        held_before: &HeldLockSnapshot,
        held_locks: &mut HeldLockStack,
        state: LockdepState,
        addr: usize,
    ) {
        self.record_edges(held_before, state.class_id);
        held_locks.push(HeldLock {
            id: state.instance_id,
            class_id: state.class_id,
            addr,
            caller: state.caller,
        });
    }
}

struct GraphState {
    lock: AtomicBool,
    graph: UnsafeCell<LockGraph>,
    classes: UnsafeCell<ClassRegistry>,
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

static NEXT_INSTANCE_ID: AtomicU32 = AtomicU32::new(1);
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

fn ensure_ids(map: &LockdepMap, class_key: *const Location<'static>) -> (u32, u32) {
    let existing_instance = map.instance_id.load(Ordering::Acquire);
    let existing_class = map.class_id.load(Ordering::Acquire);
    if existing_instance != 0 && existing_class != 0 {
        return (existing_instance, existing_class);
    }

    let _guard = GraphGuard::acquire();

    let instance_id = match map.instance_id.load(Ordering::Acquire) {
        0 => {
            let new_id = NEXT_INSTANCE_ID.fetch_add(1, Ordering::AcqRel);
            assert!(
                (new_id as usize) < MAX_LOCKS,
                "lockdep: exceeded maximum tracked lock instances ({MAX_LOCKS})"
            );
            map.instance_id.store(new_id, Ordering::Release);
            new_id
        }
        id => id,
    };

    let class_id = match map.class_id.load(Ordering::Acquire) {
        0 => {
            let key = match map.class_key.load(Ordering::Acquire) {
                ptr if ptr.is_null() => {
                    map.class_key
                        .store(class_key as *mut Location<'static>, Ordering::Release);
                    class_key
                }
                ptr => ptr as *const Location<'static>,
            };
            // SAFETY: protected by the global graph spinlock above.
            let classes = unsafe { &mut *GRAPH_STATE.classes.get() };
            let id = classes.find_or_register(key as usize);
            map.class_id.store(id, Ordering::Release);
            id
        }
        id => id,
    };

    (instance_id, class_id)
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn with_current_task_held_locks<R>(f: impl FnOnce(&HeldLockStack) -> R) -> R {
    HELD_LOCKS.with(|held_locks| f(&held_locks.borrow()))
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn with_current_task_held_locks_mut<R>(f: impl FnOnce(&mut HeldLockStack) -> R) -> R {
    HELD_LOCKS.with(|held_locks| f(&mut held_locks.borrow_mut()))
}

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
    call_interface!(KspinLockdepIf::collect_current_task_held_locks, snapshot);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
    with_current_task_held_locks(|stack| snapshot.extend(stack));
}

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
fn push_current_task_held_lock(held: HeldLock) {
    call_interface!(KspinLockdepIf::push_current_task_held_lock, held);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn push_current_task_held_lock(held: HeldLock) {
    with_current_task_held_locks_mut(|stack| stack.push(held));
}

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
fn pop_current_task_held_lock(lock_id: u32) {
    call_interface!(KspinLockdepIf::pop_current_task_held_lock, lock_id);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn pop_current_task_held_lock(lock_id: u32) {
    with_current_task_held_locks_mut(|stack| stack.pop_checked(lock_id));
}

pub fn current_task_held_lock_snapshot() -> HeldLockSnapshot {
    let mut snapshot = HeldLockSnapshot::new();
    collect_current_task_held_locks(&mut snapshot);
    snapshot
}

pub fn prepare_acquire_with_snapshot(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_checked(map, lock_kind, addr, caller, held_before).unwrap_or_else(
        |err| panic_on_lockdep_error(err, lock_kind, map, addr, caller, &held_before),
    )
}

pub fn prepare_acquire_with_snapshot_checked(
    map: &LockdepMap,
    _lock_kind: &'static str,
    _addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
) -> Result<PreparedAcquire, LockdepCheckError> {
    let class_key = caller as *const Location<'static>;
    let (instance_id, class_id) = ensure_ids(map, class_key);
    with_graph(|graph| graph.check_can_acquire(&held_before, instance_id, class_id))?;
    Ok(PreparedAcquire {
        state: LockdepState {
            instance_id,
            class_id,
            caller,
        },
        held_before,
    })
}

fn panic_on_lockdep_error(
    err: LockdepCheckError,
    lock_kind: &str,
    map: &LockdepMap,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: &HeldLockSnapshot,
) -> ! {
    let requested_id = map.lock_id().unwrap_or(0);
    let requested_class = map.class_id().unwrap_or(0);
    match err {
        LockdepCheckError::Recursive => panic!(
            "lockdep: recursive {lock_kind} acquisition detected\nrequested:\n  id={} class={} \
             addr={:#x} acquire_at={}\nalready held:\n  {}\nheld stack:\n{}",
            requested_id,
            requested_class,
            addr,
            caller,
            held_before
                .iter()
                .find(|held| held.id == requested_id)
                .or_else(|| held_before.iter().next())
                .expect("held lock snapshot unexpectedly empty"),
            HeldLockStackDisplay(held_before)
        ),
        LockdepCheckError::OrderInversion => {
            emit_lockdep_marker("lockdep: lock order inversion detected\n");
            let held = held_before
                .iter()
                .find(|held| with_graph(|graph| graph.reaches(requested_class, held.class_id)))
                .or_else(|| held_before.iter().next())
                .expect("held lock snapshot unexpectedly empty");
            panic!(
                "lockdep: lock order inversion detected\nrequested:\n  kind={} id={} class={} \
                 addr={:#x} acquire_at={}\nconflicting held lock:\n  {}\nheld stack:\n{}",
                lock_kind,
                requested_id,
                requested_class,
                addr,
                caller,
                held,
                HeldLockStackDisplay(held_before)
            );
        }
    }
}

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
fn emit_lockdep_marker(s: &str) {
    call_interface!(KspinLockdepIf::console_write_str, s);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn emit_lockdep_marker(s: &str) {
    std::eprint!("{s}");
}

pub fn finish_acquire_with_stack(
    prepared: PreparedAcquire,
    addr: usize,
    held_locks: &mut HeldLockStack,
) {
    with_graph(|graph| {
        graph.record_acquire(&prepared.held_before, held_locks, prepared.state, addr)
    });
}

pub fn finish_acquire_task(prepared: PreparedAcquire, addr: usize) {
    with_graph(|graph| graph.record_edges(&prepared.held_before, prepared.state.class_id));
    push_current_task_held_lock(HeldLock {
        id: prepared.state.instance_id,
        class_id: prepared.state.class_id,
        addr,
        caller: prepared.state.caller,
    });
}

pub fn release_from_stack(lock_id: Option<u32>, held_locks: &mut HeldLockStack) {
    if let Some(lock_id) = lock_id {
        held_locks.pop_checked(lock_id);
    }
}

pub fn release_task(lock_id: Option<u32>) {
    if let Some(lock_id) = lock_id {
        pop_current_task_held_lock(lock_id);
    }
}

pub fn force_release_task(map: &LockdepMap) {
    release_task(map.lock_id());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_lock_display_includes_id_class_addr_and_location() {
        let held = HeldLock {
            id: 7,
            class_id: 3,
            addr: 0x1234,
            caller: Location::caller(),
        };
        let rendered = held.to_string();
        assert!(rendered.contains("id=7"));
        assert!(rendered.contains("class=3"));
        assert!(rendered.contains("addr=0x1234"));
        assert!(rendered.contains("acquired_at="));
    }

    #[test]
    fn held_stack_display_marks_top_entry() {
        let caller = Location::caller();
        let mut snapshot = HeldLockSnapshot::new();
        snapshot.push(HeldLock {
            id: 1,
            class_id: 2,
            addr: 0x10,
            caller,
        });
        snapshot.push(HeldLock {
            id: 2,
            class_id: 3,
            addr: 0x20,
            caller,
        });

        let rendered = HeldLockStackDisplay(&snapshot).to_string();
        assert!(rendered.contains("[0] held: id=1"));
        assert!(rendered.contains("[1] top: id=2"));
    }
}
