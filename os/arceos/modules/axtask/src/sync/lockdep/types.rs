//! Held-lock values, snapshots, and diagnostics formatting.

use core::{fmt, panic::Location};

use super::backend::lockdep_fatal;

pub(super) const MAX_LOCK_CLASSES: usize = 1024;
pub(super) const MAX_HELD_LOCKS: usize = 32;
pub(super) const MAX_HELD_LOCK_SNAPSHOT: usize = MAX_HELD_LOCKS;
pub(super) const WORDS_PER_ROW: usize = MAX_LOCK_CLASSES.div_ceil(64);
pub(super) const LOCK_SUBCLASS_BITS: usize = 3;
pub(super) const LOCK_SUBCLASS_MASK: usize = (1 << LOCK_SUBCLASS_BITS) - 1;

pub type LockSubclass = u32;
pub const DEFAULT_LOCK_SUBCLASS: LockSubclass = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeldLockKind {
    Spin,
    SpinRwLock,
    Mutex,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The access mode represented by one held-lock entry.
pub enum HeldLockMode {
    /// A mutex or spin lock with exclusive ownership.
    Exclusive,
    /// The shared side of a read-write lock.
    Read,
    /// The exclusive side of a read-write lock.
    Write,
}

impl HeldLockMode {
    pub(super) fn allows_same_lock_nesting(self, requested: Self) -> bool {
        self == Self::Read && requested == Self::Read
    }
}

impl fmt::Display for HeldLockMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exclusive => f.write_str("exclusive"),
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
        }
    }
}

impl HeldLockKind {
    pub(super) fn from_label(label: &'static str) -> Self {
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
    pub mode: HeldLockMode,
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
            mode: HeldLockMode::Exclusive,
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
            .field("mode", &self.mode)
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
            "kind={} mode={} sleep_forbidden={} class={} addr={:#x} acquired_at={}",
            self.kind, self.mode, self.sleep_forbidden, self.class_id, self.addr, self.caller
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
        if self
            .iter()
            .find(|current| current.addr == held.addr)
            .is_some_and(|current| !current.mode.allows_same_lock_nesting(held.mode))
        {
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
pub(super) struct HeldLockSubclassSnapshot {
    pub(super) values: [LockSubclass; MAX_HELD_LOCK_SNAPSHOT],
}

impl HeldLockSubclassSnapshot {
    pub(super) fn get(&self, index: usize) -> LockSubclass {
        self.values
            .get(index)
            .copied()
            .unwrap_or(DEFAULT_LOCK_SUBCLASS)
    }
}

pub(super) struct HeldLockDisplay<'a> {
    pub(super) held: &'a HeldLock,
    pub(super) subclass: LockSubclass,
}

impl fmt::Display for HeldLockDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "kind={} mode={} sleep_forbidden={} class={} subclass={} addr={:#x} acquired_at={}",
            self.held.kind,
            self.held.mode,
            self.held.sleep_forbidden,
            self.held.class_id,
            self.subclass,
            self.held.addr,
            self.held.caller
        )
    }
}

pub(super) struct HeldLockStackDisplay<'a> {
    pub(super) snapshot: &'a HeldLockSnapshot,
    pub(super) subclasses: &'a HeldLockSubclassSnapshot,
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
