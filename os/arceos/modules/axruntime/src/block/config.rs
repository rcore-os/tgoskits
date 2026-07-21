//! Operating-system policy for the block runtime.

use core::num::NonZeroU64;

/// Default absolute watchdog budget for one accepted hardware request.
pub const DEFAULT_REQUEST_WATCHDOG_NS: u64 = 30_000_000_000;

/// Runtime-owned block policy independent of portable hardware limits.
///
/// Drivers may expose protocol deadlines for initialization and recovery, but
/// they do not choose how long the OS lets a normal request remain in flight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRuntimeConfig {
    request_watchdog_ns: NonZeroU64,
}

impl BlockRuntimeConfig {
    /// Creates a policy with a nonzero request watchdog budget.
    pub const fn new(request_watchdog_ns: NonZeroU64) -> Self {
        Self {
            request_watchdog_ns,
        }
    }

    /// Returns the normal-I/O watchdog budget in nanoseconds.
    pub const fn request_watchdog_ns(self) -> u64 {
        self.request_watchdog_ns.get()
    }
}

impl Default for BlockRuntimeConfig {
    fn default() -> Self {
        Self::new(
            NonZeroU64::new(DEFAULT_REQUEST_WATCHDOG_NS)
                .expect("the default block request watchdog is nonzero"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_watchdog_matches_the_runtime_policy() {
        assert_eq!(
            BlockRuntimeConfig::default().request_watchdog_ns(),
            DEFAULT_REQUEST_WATCHDOG_NS
        );
    }
}
