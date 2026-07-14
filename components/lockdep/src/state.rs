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

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
use ax_crate_interface::call_interface;
use ax_kspin::IrqGuard;

const MAX_LOCK_CLASSES: usize = 1024;
const MAX_HELD_LOCKS: usize = 32;
const MAX_HELD_LOCK_SNAPSHOT: usize = MAX_HELD_LOCKS;
const WORDS_PER_ROW: usize = MAX_LOCK_CLASSES.div_ceil(64);
const LOCK_SUBCLASS_BITS: usize = 3;
const LOCK_SUBCLASS_MASK: usize = (1 << LOCK_SUBCLASS_BITS) - 1;

pub type LockSubclass = u32;
pub const DEFAULT_LOCK_SUBCLASS: LockSubclass = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeldLockKind {
    Spin,
    SpinRwLock,
    Mutex,
    Other,
}

impl HeldLockKind {
    fn from_label(label: &'static str) -> Self {
        match label {
            "spin" | "spin lock" => Self::Spin,
            "spin-rwlock" | "spin rwlock" => Self::SpinRwLock,
            "mutex" => Self::Mutex,
            _ => Self::Other,
        }
    }
}

impl fmt::Display for HeldLockKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spin => f.write_str("spin"),
            Self::SpinRwLock => f.write_str("spin-rwlock"),
            Self::Mutex => f.write_str("mutex"),
            Self::Other => f.write_str("other"),
        }
    }
}

#[derive(Clone, Copy)]
pub struct HeldLock {
    pub class_id: u32,
    pub kind: HeldLockKind,
    pub sleep_forbidden: bool,
    pub addr: usize,
    pub caller: &'static Location<'static>,
}

impl HeldLock {
    #[track_caller]
    const fn placeholder() -> Self {
        Self {
            class_id: 0,
            kind: HeldLockKind::Other,
            sleep_forbidden: false,
            addr: 0,
            caller: Location::caller(),
        }
    }
}

impl fmt::Debug for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeldLock")
            .field("class_id", &self.class_id)
            .field("kind", &self.kind)
            .field("sleep_forbidden", &self.sleep_forbidden)
            .field("addr", &format_args!("{:#x}", self.addr))
            .field("caller", &self.caller)
            .finish()
    }
}

impl fmt::Display for HeldLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "kind={} sleep_forbidden={} class={} addr={:#x} acquired_at={}",
            self.kind, self.sleep_forbidden, self.class_id, self.addr, self.caller
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
    pub fn contains_addr(&self, addr: usize) -> bool {
        self.iter().any(|held| held.addr == addr)
    }

    pub fn push(&mut self, held: HeldLock) {
        if self.contains_addr(held.addr) {
            lockdep_fatal(format_args!(
                "lockdep: duplicate held lock push while acquiring {:?}; stack {:?}",
                held, self
            ));
        }
        if self.depth >= MAX_HELD_LOCKS {
            lockdep_fatal(format_args!(
                "lockdep: held lock stack overflow while acquiring {:?}",
                held
            ));
        }
        self.entries[self.depth] = held;
        self.depth += 1;
    }

    pub fn pop_checked(&mut self, addr: usize) {
        if self.depth == 0 {
            lockdep_fatal(format_args!(
                "lockdep: releasing lock {addr:#x} with empty held lock stack"
            ));
        }
        let top = self.entries[self.depth - 1];
        if top.addr != addr {
            lockdep_fatal(format_args!(
                "lockdep: unlock order violation, releasing addr={:#x} while top of stack is {:?}",
                addr, top
            ));
        }
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
    // duplicate lock addresses are filtered out when extending/pushing into it.
    pub fn contains_addr(&self, addr: usize) -> bool {
        self.iter().any(|held| held.addr == addr)
    }

    pub fn extend(&mut self, stack: &HeldLockStack) {
        for held in stack.iter() {
            self.push(held);
        }
    }

    pub fn push(&mut self, held: HeldLock) {
        if self.contains_addr(held.addr) {
            return;
        }

        if self.depth >= MAX_HELD_LOCK_SNAPSHOT {
            lockdep_fatal(format_args!(
                "lockdep: combined held lock snapshot overflow while acquiring {:?}",
                held
            ));
        }
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

impl fmt::Display for HeldLockSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.depth == 0 {
            return f.write_str("[]");
        }

        f.write_str("[")?;
        for (index, held) in self.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            let relation = if index + 1 == self.depth {
                "top"
            } else {
                "held"
            };
            write!(f, "#{index} {relation}: {held}")?;
        }
        f.write_str("]")
    }
}

#[derive(Clone, Copy)]
struct HeldLockSubclassSnapshot {
    values: [LockSubclass; MAX_HELD_LOCK_SNAPSHOT],
}

impl HeldLockSubclassSnapshot {
    fn get(&self, index: usize) -> LockSubclass {
        self.values
            .get(index)
            .copied()
            .unwrap_or(DEFAULT_LOCK_SUBCLASS)
    }
}

struct HeldLockDisplay<'a> {
    held: &'a HeldLock,
    subclass: LockSubclass,
}

impl fmt::Display for HeldLockDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "kind={} sleep_forbidden={} class={} subclass={} addr={:#x} acquired_at={}",
            self.held.kind,
            self.held.sleep_forbidden,
            self.held.class_id,
            self.subclass,
            self.held.addr,
            self.held.caller
        )
    }
}

struct HeldLockStackDisplay<'a> {
    snapshot: &'a HeldLockSnapshot,
    subclasses: &'a HeldLockSubclassSnapshot,
}

impl fmt::Display for HeldLockStackDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.snapshot.depth == 0 {
            return write!(f, "  (empty)");
        }

        for (index, held) in self.snapshot.iter().enumerate() {
            let relation = if index + 1 == self.snapshot.depth {
                "top"
            } else {
                "held"
            };
            writeln!(
                f,
                "  [{}] {}: {}",
                index,
                relation,
                HeldLockDisplay {
                    held: &held,
                    subclass: self.subclasses.get(index),
                }
            )?;
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
    fn pop_current_task_held_lock(lock_addr: usize);
    fn console_write_str(s: &str);
    fn fatal() -> !;
}

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
pub enum LockdepCheckError {
    Recursive,
    OrderInversion,
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
    ) -> Result<(), LockdepCheckError> {
        if held_locks.contains_addr(addr) {
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
        let max_id = NEXT_CLASS_ID
            .load(Ordering::Acquire)
            .min(MAX_LOCK_CLASSES as u32);

        for held in held_before.iter() {
            self.add_order(held.class_id, class_id, max_id);
        }
    }

    fn record_acquire(
        &mut self,
        held_before: &HeldLockSnapshot,
        held_locks: &mut HeldLockStack,
        prepared: PreparedAcquire,
        addr: usize,
    ) {
        self.record_edges(held_before, prepared.state.class_id);
        held_locks.push(HeldLock {
            class_id: prepared.state.class_id,
            kind: prepared.kind,
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
    _irq_guard: IrqGuard,
}

impl GraphGuard {
    fn acquire() -> Self {
        let irq_guard = IrqGuard::new();
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
            _irq_guard: irq_guard,
        }
    }
}

impl Drop for GraphGuard {
    fn drop(&mut self) {
        GRAPH_STATE.lock.store(false, Ordering::Release);
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
fn pop_current_task_held_lock(lock_addr: usize) {
    call_interface!(KspinLockdepIf::pop_current_task_held_lock, lock_addr);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn pop_current_task_held_lock(lock_addr: usize) {
    with_current_task_held_locks_mut(|stack| stack.pop_checked(lock_addr));
}

#[cfg(all(feature = "task-context", not(any(test, doctest))))]
fn lockdep_fatal(message: fmt::Arguments<'_>) -> ! {
    let _oops_guard = axpanic::enter_oops();

    struct ConsoleWriter;

    impl fmt::Write for ConsoleWriter {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            emergency_write_str(s);
            Ok(())
        }
    }

    let mut writer = ConsoleWriter;
    let _ = fmt::Write::write_fmt(&mut writer, message);
    let _ = fmt::Write::write_str(&mut writer, "\n");
    emergency_write_str("lockdep fatal violation\n");
    call_interface!(KspinLockdepIf::fatal)
}

#[cfg(all(
    feature = "task-context",
    not(any(test, doctest)),
    target_arch = "riscv64"
))]
fn emergency_write_str(s: &str) {
    for &byte in s.as_bytes() {
        #[allow(deprecated)]
        {
            sbi_rt::legacy::console_putchar(byte as usize);
        }
    }
}

#[cfg(all(
    feature = "task-context",
    not(any(test, doctest)),
    not(target_arch = "riscv64")
))]
fn emergency_write_str(s: &str) {
    call_interface!(KspinLockdepIf::console_write_str, s);
}

#[cfg(any(
    test,
    doctest,
    all(not(target_os = "none"), not(feature = "task-context"))
))]
fn lockdep_fatal(message: fmt::Arguments<'_>) -> ! {
    panic!("{message}")
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
    prepare_acquire_with_snapshot_nested(
        map,
        lock_kind,
        addr,
        caller,
        held_before,
        DEFAULT_LOCK_SUBCLASS,
    )
}

pub fn prepare_acquire_with_snapshot_nested(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_nested_with_sleep(
        map,
        lock_kind,
        addr,
        caller,
        held_before,
        subclass,
        true,
    )
}

pub fn prepare_acquire_with_snapshot_nested_with_sleep(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
    sleep_forbidden: bool,
) -> PreparedAcquire {
    prepare_acquire_with_snapshot_result(
        map,
        lock_kind,
        addr,
        caller,
        held_before,
        subclass,
        sleep_forbidden,
    )
    .unwrap_or_else(|(err, state)| {
        fatal_on_lockdep_error(err, lock_kind, state, addr, &held_before)
    })
}

pub fn prepare_acquire_with_snapshot_checked(
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

pub fn prepare_acquire_with_snapshot_checked_nested(
    map: &LockdepMap,
    _lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
) -> Result<PreparedAcquire, LockdepCheckError> {
    prepare_acquire_with_snapshot_result(map, _lock_kind, addr, caller, held_before, subclass, true)
        .map_err(|(err, _state)| err)
}

fn prepare_acquire_with_snapshot_result(
    map: &LockdepMap,
    lock_kind: &'static str,
    addr: usize,
    caller: &'static Location<'static>,
    held_before: HeldLockSnapshot,
    subclass: LockSubclass,
    sleep_forbidden: bool,
) -> Result<PreparedAcquire, (LockdepCheckError, LockdepState)> {
    let class_key = caller as *const Location<'static>;
    let state = ensure_class(map, class_key, subclass);
    with_graph(|graph| graph.check_can_acquire(&held_before, addr, state.class_id))
        .map_err(|err| (err, state))?;
    Ok(PreparedAcquire {
        state,
        held_before,
        kind: HeldLockKind::from_label(lock_kind),
        sleep_forbidden,
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

pub fn finish_acquire_with_stack(
    prepared: PreparedAcquire,
    addr: usize,
    held_locks: &mut HeldLockStack,
) {
    with_graph(|graph| graph.record_acquire(&prepared.held_before, held_locks, prepared, addr));
}

pub fn finish_acquire_task(prepared: PreparedAcquire, addr: usize) {
    with_graph(|graph| graph.record_edges(&prepared.held_before, prepared.state.class_id));
    push_current_task_held_lock(HeldLock {
        class_id: prepared.state.class_id,
        kind: prepared.kind,
        sleep_forbidden: prepared.sleep_forbidden,
        addr,
        caller: prepared.state.caller,
    });
}

pub fn release_from_stack(lock_addr: usize, held_locks: &mut HeldLockStack) {
    held_locks.pop_checked(lock_addr);
}

pub fn release_task(lock_addr: usize) {
    pop_current_task_held_lock(lock_addr);
}

pub fn force_release_task(lock_addr: usize) {
    release_task(lock_addr);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_lock_display_includes_class_addr_and_location() {
        let held = HeldLock {
            class_id: 3,
            kind: HeldLockKind::Spin,
            sleep_forbidden: true,
            addr: 0x1234,
            caller: Location::caller(),
        };
        let rendered = held.to_string();
        assert!(rendered.contains("kind=spin"));
        assert!(rendered.contains("sleep_forbidden=true"));
        assert!(rendered.contains("class=3"));
        assert!(rendered.contains("addr=0x1234"));
        assert!(rendered.contains("acquired_at="));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn subclass_support_does_not_increase_held_lock_state_size() {
        assert_eq!(core::mem::size_of::<HeldLock>(), 24);
        assert_eq!(core::mem::size_of::<HeldLockStack>(), 776);
        assert_eq!(core::mem::size_of::<HeldLockSnapshot>(), 776);
        assert_eq!(core::mem::size_of::<PreparedAcquire>(), 800);
    }

    #[test]
    fn held_stack_display_marks_top_entry() {
        let caller = Location::caller();
        let mut snapshot = HeldLockSnapshot::new();
        snapshot.push(HeldLock {
            class_id: 2,
            kind: HeldLockKind::Spin,
            sleep_forbidden: true,
            addr: 0x10,
            caller,
        });
        snapshot.push(HeldLock {
            class_id: 3,
            kind: HeldLockKind::Mutex,
            sleep_forbidden: false,
            addr: 0x20,
            caller,
        });

        let rendered = HeldLockStackDisplay {
            snapshot: &snapshot,
            subclasses: &HeldLockSubclassSnapshot {
                values: [DEFAULT_LOCK_SUBCLASS; MAX_HELD_LOCK_SNAPSHOT],
            },
        }
        .to_string();
        assert!(rendered.contains("[0] held: kind=spin sleep_forbidden=true class=2"));
        assert!(rendered.contains("[1] top: kind=mutex sleep_forbidden=false class=3"));
    }

    #[test]
    fn dynamic_lock_instances_do_not_consume_class_slots() {
        let locks: Vec<_> = (0..(MAX_LOCK_CLASSES + 128))
            .map(|_| LockdepMap::new_dynamic())
            .collect();

        for lock in &locks {
            let prepared = prepare_acquire_with_snapshot_checked(
                lock,
                "test lock",
                lock as *const _ as usize,
                Location::caller(),
                HeldLockSnapshot::new(),
            )
            .unwrap();
            assert_ne!(prepared.class_id(), 0);
        }
    }

    #[test]
    fn subclass_tracks_same_base_class_nesting() {
        fn prepare_with_subclass(
            map: &LockdepMap,
            held_before: HeldLockSnapshot,
            subclass: LockSubclass,
        ) -> PreparedAcquire {
            prepare_acquire_with_snapshot_checked_nested(
                map,
                "test lock",
                map as *const _ as usize,
                Location::caller(),
                held_before,
                subclass,
            )
            .unwrap()
        }

        let parent = LockdepMap::new_dynamic();
        let child = LockdepMap::new_dynamic();
        let parent_acquire =
            prepare_with_subclass(&parent, HeldLockSnapshot::new(), DEFAULT_LOCK_SUBCLASS);
        let parent_class = parent_acquire.class_id();
        let child_acquire = prepare_with_subclass(
            &child,
            HeldLockSnapshot {
                depth: 1,
                entries: {
                    let mut entries = [HeldLock::placeholder(); MAX_HELD_LOCK_SNAPSHOT];
                    entries[0] = HeldLock {
                        class_id: parent_class,
                        kind: HeldLockKind::Spin,
                        sleep_forbidden: true,
                        addr: &parent as *const _ as usize,
                        caller: Location::caller(),
                    };
                    entries
                },
            },
            1,
        );
        assert_eq!(class_subclass(parent_class), DEFAULT_LOCK_SUBCLASS);
        assert_eq!(class_subclass(child_acquire.class_id()), 1);

        let mut held_locks = HeldLockStack::new();
        finish_acquire_with_stack(
            parent_acquire,
            &parent as *const _ as usize,
            &mut held_locks,
        );
        finish_acquire_with_stack(child_acquire, &child as *const _ as usize, &mut held_locks);
        release_from_stack(&child as *const _ as usize, &mut held_locks);
        release_from_stack(&parent as *const _ as usize, &mut held_locks);

        let nested_held = HeldLockSnapshot {
            depth: 1,
            entries: {
                let mut entries = [HeldLock::placeholder(); MAX_HELD_LOCK_SNAPSHOT];
                entries[0] = HeldLock {
                    class_id: child_acquire.class_id(),
                    kind: HeldLockKind::Spin,
                    sleep_forbidden: true,
                    addr: &child as *const _ as usize,
                    caller: Location::caller(),
                };
                entries
            },
        };
        let reverse = prepare_acquire_with_snapshot_checked_nested(
            &parent,
            "test lock",
            &parent as *const _ as usize,
            Location::caller(),
            nested_held,
            DEFAULT_LOCK_SUBCLASS,
        );
        assert!(matches!(reverse, Err(LockdepCheckError::OrderInversion)));
    }
}
