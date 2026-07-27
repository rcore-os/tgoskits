//! Resource limits.

use core::ops::{Index, IndexMut};

use linux_raw_sys::general::{
    RLIM_NLIMITS, RLIMIT_DATA, RLIMIT_MSGQUEUE, RLIMIT_NOFILE, RLIMIT_STACK,
};

/// The maximum number of open files
pub const AX_FILE_LIMIT: usize = 1024;

/// Default (and hard) `RLIMIT_MSGQUEUE`: the per-user byte ceiling on POSIX
/// message queues, matching Linux `MQ_BYTES_MAX` (`include/uapi/linux/mqueue.h`,
/// 819200) as seeded by `INIT_RLIMITS`.
pub const MQ_BYTES_MAX: u64 = 819200;

/// The limit for a specific resource
#[derive(Default)]
pub struct Rlimit {
    /// The current limit for the resource (soft)
    pub current: u64,
    /// The maximum limit for the resource (hard)
    pub max: u64,
}

impl Rlimit {
    /// Creates a new `Rlimit` with the specified soft and hard limits.
    pub fn new(soft: u64, hard: u64) -> Self {
        Self {
            current: soft,
            max: hard,
        }
    }
}

impl From<u64> for Rlimit {
    fn from(value: u64) -> Self {
        Self {
            current: value,
            max: value,
        }
    }
}

/// Process resource limits
pub struct Rlimits([Rlimit; RLIM_NLIMITS as usize]);

impl Default for Rlimits {
    fn default() -> Self {
        let mut result = Self(Default::default());
        // Match the Linux default (8 MiB) so applications like PostgreSQL
        // that compute safe recursion/stack-depth limits from getrlimit
        // get a consistent answer. USER_STACK_SIZE is kept in sync so the
        // advertised limit matches the mapped stack VMA.
        result[RLIMIT_STACK] = (crate::config::USER_STACK_SIZE as u64).into();
        result[RLIMIT_NOFILE] = (AX_FILE_LIMIT as u64).into();
        // Linux default: RLIMIT_DATA is unlimited
        result[RLIMIT_DATA] = Rlimit::new(u64::MAX, u64::MAX);
        // Linux `INIT_RLIMITS` seeds RLIMIT_MSGQUEUE with MQ_BYTES_MAX
        // (`include/asm-generic/resource.h`, `include/uapi/linux/mqueue.h`):
        // the per-user ceiling on bytes held across all that user's POSIX
        // message queues. mq_open charges the queue's mq_bytes against it.
        result[RLIMIT_MSGQUEUE] = Rlimit::new(MQ_BYTES_MAX, MQ_BYTES_MAX);
        result
    }
}

impl Index<u32> for Rlimits {
    type Output = Rlimit;

    fn index(&self, index: u32) -> &Self::Output {
        &self.0[index as usize]
    }
}

impl IndexMut<u32> for Rlimits {
    fn index_mut(&mut self, index: u32) -> &mut Self::Output {
        &mut self.0[index as usize]
    }
}

#[cfg(axtest)]
pub(crate) fn resource_limit_defaults_hold_for_test() -> bool {
    let mut limits = Rlimits::default();
    limits[RLIMIT_NOFILE] = Rlimit::new(7, 9);

    limits[RLIMIT_STACK].current == crate::config::USER_STACK_SIZE as u64
        && limits[RLIMIT_STACK].max == crate::config::USER_STACK_SIZE as u64
        && limits[RLIMIT_DATA].current == u64::MAX
        && limits[RLIMIT_DATA].max == u64::MAX
        && limits[RLIMIT_NOFILE].current == 7
        && limits[RLIMIT_NOFILE].max == 9
        && Rlimit::from(11).current == 11
        && Rlimit::from(11).max == 11
}
