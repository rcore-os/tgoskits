//! A host-side mock [`CgroupProvider`] for unit tests.
//!
//! The real provider lives in the StarryOS kernel. For host tests we need a
//! minimal stand-in that tracks per-pid cgroup assignment and zombie state.
//!
//! cgroup global state (`init`, the factory registry, the provider cell) is
//! process-wide and may only be set up once, so [`ensure_init`] funnels every
//! test through a single [`std::sync::OnceLock`].

use alloc::{boxed::Box, sync::Arc};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, OnceLock},
};

use crate::{CgroupNode, CgroupProvider};

/// Mock provider state: pid → cgroup assignment plus a zombie set.
#[derive(Default)]
struct MockState {
    cgroups: BTreeMap<u32, Arc<CgroupNode>>,
    zombies: BTreeSet<u32>,
}

/// Host mock of the kernel [`CgroupProvider`].
pub struct MockProvider {
    state: Mutex<MockState>,
}

impl MockProvider {
    fn new() -> Self {
        Self {
            state: Mutex::new(MockState::default()),
        }
    }

    /// Mark a pid as a zombie (so migration rejects it).
    pub fn set_zombie(&self, pid: u32, zombie: bool) {
        let mut state = self.state.lock().unwrap();
        if zombie {
            state.zombies.insert(pid);
        } else {
            state.zombies.remove(&pid);
        }
    }

    /// Forget all per-pid assignments and zombies (per-test reset).
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.cgroups.clear();
        state.zombies.clear();
    }
}

impl CgroupProvider for MockProvider {
    fn is_zombie(&self, pid: u32) -> bool {
        self.state.lock().unwrap().zombies.contains(&pid)
    }

    fn get_cgroup(&self, pid: u32) -> Option<Arc<CgroupNode>> {
        self.state.lock().unwrap().cgroups.get(&pid).cloned()
    }

    fn set_cgroup(&self, pid: u32, cgroup: Arc<CgroupNode>) {
        self.state.lock().unwrap().cgroups.insert(pid, cgroup);
    }
}

static MOCK: OnceLock<&'static MockProvider> = OnceLock::new();

/// Initialize the cgroup subsystem with the mock provider exactly once,
/// and return the leaked `&'static` mock for per-test manipulation.
///
/// Safe to call from every test; only the first call performs setup.
pub fn ensure_init() -> &'static MockProvider {
    MOCK.get_or_init(|| {
        crate::init();
        let mock: &'static MockProvider = Box::leak(Box::new(MockProvider::new()));
        crate::register_provider(mock);
        mock
    })
}
