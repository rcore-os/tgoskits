//! A host-side mock [`CgroupProvider`] for unit tests.
//!
//! The real provider lives in the StarryOS kernel. For host tests we need a
//! minimal stand-in that tracks per-pid cgroup assignment and zombie state.
//!
//! cgroup global state (`init`, the factory registry, the provider cell) is
//! process-wide and may only be set up once, so [`ensure_init`] funnels every
//! test through a single [`std::sync::OnceLock`].

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
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
    /// UID returned by `current_uid`; defaults to 0 (root) so existing tests
    /// retain full write rights unless they opt into an unprivileged caller.
    current_uid: u32,
    /// Records cgroup paths for which `notify_populated_changed` fired, in
    /// order, so tests can assert populated-flip edge semantics without a real
    /// inotify backend.
    populated_notifications: Vec<String>,
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

    /// Set the UID that `current_uid` will report (delegation tests).
    pub fn set_current_uid(&self, uid: u32) {
        self.state.lock().unwrap().current_uid = uid;
    }

    /// Forget all per-pid assignments and zombies (per-test reset).
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.cgroups.clear();
        state.zombies.clear();
        state.current_uid = 0;
        state.populated_notifications.clear();
    }

    /// cgroup paths for which `notify_populated_changed` has fired since reset.
    pub fn populated_notifications(&self) -> Vec<String> {
        self.state.lock().unwrap().populated_notifications.clone()
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

    fn current_uid(&self) -> u32 {
        self.state.lock().unwrap().current_uid
    }

    fn notify_populated_changed(&self, cgroup_path: &str) {
        self.state
            .lock()
            .unwrap()
            .populated_notifications
            .push(cgroup_path.into());
    }
}

static MOCK: OnceLock<&'static MockProvider> = OnceLock::new();

/// Serializes tests that mutate the process-global cgroup tree (root,
/// registry, membership). The hierarchy is a singleton, so concurrent
/// structural mutation from parallel test threads is logically invalid;
/// every global-touching test holds this for its duration.
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the global-state serialization guard. Hold it for the whole test.
pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    // Recover from a poisoned lock: a panicking test still leaves the tree
    // usable for the next, and we only guard structural serialization.
    TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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
