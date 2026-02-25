//! Resource limits.

use core::ops::{Index, IndexMut};

use linux_raw_sys::general::{RLIM_NLIMITS, RLIMIT_NOFILE, RLIMIT_STACK};

/// The maximum number of open files
pub const AX_FILE_LIMIT: usize = 1024;

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
        result[RLIMIT_STACK] = (crate::config::USER_STACK_SIZE as u64).into();
        result[RLIMIT_NOFILE] = (AX_FILE_LIMIT as u64).into();
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
