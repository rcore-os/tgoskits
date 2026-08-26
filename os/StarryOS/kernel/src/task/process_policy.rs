//! Process-wide resource limits and compatibility policy.

use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use linux_raw_sys::general::RLIM_NLIMITS;

use super::{ProcessData, Rlimit, Rlimits};
use crate::sync::{PiMutex, PiMutexGuard};

struct AtomicRlimit {
    current: AtomicU64,
    max: AtomicU64,
}

impl AtomicRlimit {
    fn new(limit: Rlimit) -> Self {
        Self {
            current: AtomicU64::new(limit.current),
            max: AtomicU64::new(limit.max),
        }
    }

    fn snapshot(&self) -> Rlimit {
        // Linux's task_rlimit() readers deliberately observe each word
        // independently. These values do not publish any other state.
        Rlimit::new(
            self.current.load(Ordering::Relaxed),
            self.max.load(Ordering::Relaxed),
        )
    }

    fn replace(&self, limit: Rlimit) {
        self.max.store(limit.max, Ordering::Relaxed);
        self.current.store(limit.current, Ordering::Relaxed);
    }
}

struct ProcessResourceLimits {
    writer: PiMutex<()>,
    entries: [AtomicRlimit; RLIM_NLIMITS as usize],
}

impl ProcessResourceLimits {
    fn new() -> Self {
        let defaults = Rlimits::default();
        Self {
            writer: PiMutex::new(()),
            entries: core::array::from_fn(|index| AtomicRlimit::new(defaults[index as u32])),
        }
    }

    fn current(&self, resource: u32) -> u64 {
        self.entries[resource as usize]
            .current
            .load(Ordering::Relaxed)
    }

    fn limit(&self, resource: u32) -> Rlimit {
        self.entries[resource as usize].snapshot()
    }

    fn update(&self, resource: u32) -> ResourceLimitUpdate<'_> {
        ResourceLimitUpdate {
            entry: &self.entries[resource as usize],
            _guard: self.writer.lock(),
        }
    }
}

pub(crate) struct ResourceLimitUpdate<'a> {
    entry: &'a AtomicRlimit,
    _guard: PiMutexGuard<'a, ()>,
}

impl ResourceLimitUpdate<'_> {
    pub(crate) fn snapshot(&self) -> Rlimit {
        self.entry.snapshot()
    }

    pub(crate) fn replace(self, limit: Rlimit) {
        self.entry.replace(limit);
    }
}

pub(super) struct ProcessPolicyState {
    rlimits: ProcessResourceLimits,
    umask: AtomicU32,
    dumpable: AtomicI32,
    thp_disable: AtomicU32,
    personality: AtomicUsize,
}

impl ProcessPolicyState {
    pub(super) fn new() -> Self {
        Self {
            rlimits: ProcessResourceLimits::new(),
            umask: AtomicU32::new(0o022),
            dumpable: AtomicI32::new(1),
            thp_disable: AtomicU32::new(0),
            personality: AtomicUsize::new(0),
        }
    }

    fn rlimit_current(&self, resource: u32) -> u64 {
        self.rlimits.current(resource)
    }

    fn rlimit(&self, resource: u32) -> Rlimit {
        self.rlimits.limit(resource)
    }
}

impl ProcessData {
    pub fn rlimit(&self, resource: u32) -> Rlimit {
        self.policy.rlimit(resource)
    }

    pub(crate) fn rlimit_update(&self, resource: u32) -> ResourceLimitUpdate<'_> {
        self.policy.rlimits.update(resource)
    }

    pub fn rlimit_current(&self, resource: u32) -> u64 {
        self.policy.rlimit_current(resource)
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

#[cfg(axtest)]
fn resource_limit_read_is_nonblocking_for_test() -> bool {
    use alloc::{string::String, sync::Arc};
    use core::sync::atomic::{AtomicBool, Ordering};

    use linux_raw_sys::general::RLIMIT_RTTIME;

    let policy = Arc::new(ProcessPolicyState::new());
    let started = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let worker_policy = Arc::clone(&policy);
    let worker_started = Arc::clone(&started);
    let worker_completed = Arc::clone(&completed);
    let limits = policy.rlimits.writer.lock();
    let worker = super::try_spawn_kernel_thread(
        move || {
            worker_started.store(true, Ordering::Release);
            worker_completed.store(
                worker_policy.rlimit_current(RLIMIT_RTTIME) == u64::MAX,
                Ordering::Release,
            );
        },
        String::from("rlimit-read-fast-path"),
    )
    .expect("failed to spawn resource-limit read test worker");

    while !started.load(Ordering::Acquire) {
        super::yield_now();
    }
    for _ in 0..4 {
        super::yield_now();
    }
    let completed_while_limits_locked = completed.load(Ordering::Acquire);

    drop(limits);
    super::join_kernel_thread(worker);
    completed_while_limits_locked
}

#[cfg(all(test, axtest))]
mod axtests {
    #[axtest::axtest]
    fn resource_limit_read_is_nonblocking() {
        assert!(super::resource_limit_read_is_nonblocking_for_test());
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::ProcessPolicyState;
    use crate::sync::PiMutex;

    #[test]
    fn resource_limits_use_a_sleepable_pi_lock() {
        fn assert_pi_mutex<T>(_: &PiMutex<T>) {}
        fn assert_policy_lock_types(policy: &ProcessPolicyState) {
            assert_pi_mutex(&policy.rlimits.writer);
        }

        let _ = assert_policy_lock_types as fn(&ProcessPolicyState);
    }
}
