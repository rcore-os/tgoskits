//! Process-wide resource limits and compatibility policy.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

use ax_kspin::{SpinRwLock, SpinRwLockReadGuard, SpinRwLockWriteGuard};

use super::{ProcessData, Rlimits};

pub(super) struct ProcessPolicyState {
    rlimits: SpinRwLock<Rlimits>,
    umask: AtomicU32,
    membarrier_state: AtomicU32,
    dumpable: AtomicI32,
    thp_disable: AtomicU32,
    personality: AtomicUsize,
}

impl ProcessPolicyState {
    pub(super) fn new() -> Self {
        Self {
            rlimits: SpinRwLock::default(),
            umask: AtomicU32::new(0o022),
            membarrier_state: AtomicU32::new(0),
            dumpable: AtomicI32::new(1),
            thp_disable: AtomicU32::new(0),
            personality: AtomicUsize::new(0),
        }
    }
}

impl ProcessData {
    pub fn rlimits(&self) -> SpinRwLockReadGuard<'_, Rlimits> {
        self.policy.rlimits.read()
    }

    pub fn rlimits_mut(&self) -> SpinRwLockWriteGuard<'_, Rlimits> {
        self.policy.rlimits.write()
    }

    pub fn umask(&self) -> u32 {
        self.policy.umask.load(Ordering::SeqCst)
    }

    pub fn set_umask(&self, umask: u32) {
        self.policy.umask.store(umask, Ordering::SeqCst);
    }

    pub fn replace_umask(&self, umask: u32) -> u32 {
        self.policy.umask.swap(umask, Ordering::SeqCst)
    }

    pub fn membarrier_state(&self) -> u32 {
        self.policy.membarrier_state.load(Ordering::SeqCst)
    }

    pub fn register_membarrier_state(&self, state: u32) {
        self.policy
            .membarrier_state
            .fetch_or(state, Ordering::SeqCst);
    }

    pub fn dumpable(&self) -> i32 {
        self.policy.dumpable.load(Ordering::SeqCst)
    }

    pub fn set_dumpable(&self, dumpable: i32) {
        self.policy.dumpable.store(dumpable, Ordering::SeqCst);
    }

    pub fn thp_disable(&self) -> u32 {
        self.policy.thp_disable.load(Ordering::SeqCst)
    }

    pub fn set_thp_disable(&self, thp_disable: u32) {
        self.policy.thp_disable.store(thp_disable, Ordering::SeqCst);
    }

    pub fn personality(&self) -> usize {
        self.policy.personality.load(Ordering::Acquire)
    }

    pub fn replace_personality(&self, personality: usize) -> usize {
        self.policy.personality.swap(personality, Ordering::AcqRel)
    }
}
