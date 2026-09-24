use alloc::{string::String, vec::Vec};
use core::{
    fmt::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::{FCNTL_LOCKS, FLOCK_LOCKS, FLockEntry, FlockEntry, RwLock};

pub(super) struct LockTiming {
    calls: AtomicU64,
    wait_ns: AtomicU64,
    held_ns: AtomicU64,
}

impl LockTiming {
    const fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
            wait_ns: AtomicU64::new(0),
            held_ns: AtomicU64::new(0),
        }
    }

    pub(super) fn record(&self, requested: Duration, acquired: Duration, released: Duration) {
        let wait = acquired.saturating_sub(requested).as_nanos();
        let held = released.saturating_sub(acquired).as_nanos();
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.wait_ns.fetch_add(wait.min(u64::MAX as u128) as u64, Ordering::Relaxed);
        self.held_ns.fetch_add(held.min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }

    fn append(&self, output: &mut String, name: &str) {
        writeln!(output, "{name}_calls {}", self.calls.load(Ordering::Relaxed)).unwrap();
        writeln!(output, "{name}_wait_ns {}", self.wait_ns.load(Ordering::Relaxed)).unwrap();
        writeln!(output, "{name}_held_ns {}", self.held_ns.load(Ordering::Relaxed)).unwrap();
    }
}

pub(super) static FCNTL_INDEX: LockTiming = LockTiming::new();
pub(super) static FLOCK_INDEX: LockTiming = LockTiming::new();
pub(super) static FCNTL_REAP: LockTiming = LockTiming::new();
pub(super) static FLOCK_REAP: LockTiming = LockTiming::new();
pub(super) static POSIX_SET: LockTiming = LockTiming::new();
pub(super) static OFD_SET: LockTiming = LockTiming::new();
pub(super) static GETLK: LockTiming = LockTiming::new();
pub(super) static GETLK_CLEANUP: LockTiming = LockTiming::new();
pub(super) static FLOCK: LockTiming = LockTiming::new();

pub(crate) fn render_file_lock_metrics() -> String {
    let mut output = String::new();
    writeln!(output, "inode_key_size {}", core::mem::size_of::<crate::file::InodeKey>()).unwrap();
    writeln!(output, "fcntl_entry_size {}", core::mem::size_of::<FLockEntry>()).unwrap();
    writeln!(output, "fcntl_state_size {}", core::mem::size_of::<RwLock<Vec<FLockEntry>>>())
        .unwrap();
    writeln!(output, "flock_entry_size {}", core::mem::size_of::<FlockEntry>()).unwrap();
    writeln!(output, "flock_state_size {}", core::mem::size_of::<RwLock<Vec<FlockEntry>>>())
        .unwrap();
    for (name, timing) in [
        ("fcntl_index", &FCNTL_INDEX),
        ("flock_index", &FLOCK_INDEX),
        ("fcntl_reap", &FCNTL_REAP),
        ("flock_reap", &FLOCK_REAP),
        ("posix_set", &POSIX_SET),
        ("ofd_set", &OFD_SET),
        ("getlk", &GETLK),
        ("getlk_cleanup", &GETLK_CLEANUP),
        ("flock", &FLOCK),
    ] {
        timing.append(&mut output, name);
    }

    let fcntl = FCNTL_LOCKS.read();
    let (fcntl_records, fcntl_capacity) = fcntl.states.values().fold((0, 0), |counts, state| {
        let entries = state.read();
        (counts.0 + entries.len(), counts.1 + entries.capacity())
    });
    writeln!(output, "fcntl_states {}", fcntl.states.len()).unwrap();
    writeln!(output, "fcntl_idle {}", fcntl.idle.len()).unwrap();
    writeln!(output, "fcntl_records {fcntl_records}").unwrap();
    writeln!(output, "fcntl_capacity {fcntl_capacity}").unwrap();
    drop(fcntl);

    let flock = FLOCK_LOCKS.read();
    let (flock_records, flock_capacity) = flock.states.values().fold((0, 0), |counts, state| {
        let entries = state.read();
        (counts.0 + entries.len(), counts.1 + entries.capacity())
    });
    writeln!(output, "flock_states {}", flock.states.len()).unwrap();
    writeln!(output, "flock_idle {}", flock.idle.len()).unwrap();
    writeln!(output, "flock_records {flock_records}").unwrap();
    writeln!(output, "flock_capacity {flock_capacity}").unwrap();
    output
}
