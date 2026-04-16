use core::{fmt, panic::Location};

const MAX_HELD_LOCKS: usize = 32;

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
