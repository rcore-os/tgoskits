//! Per-host USB topology refresh ownership state.

use core::time::Duration;

/// Round-robin selection state for USB hosts awaiting topology work.
#[derive(Debug, Default)]
pub(crate) struct HostRefreshCursor {
    next: usize,
}

impl HostRefreshCursor {
    /// Claims the first runnable host at or after the rotating cursor.
    ///
    /// `claim` performs the host-specific eligibility check and state
    /// transition. A successful claim advances the cursor before slow device
    /// access begins, so a busy or continuously dirty host cannot monopolize
    /// later service passes.
    pub(crate) fn claim_next(
        &mut self,
        host_count: usize,
        mut claim: impl FnMut(usize) -> bool,
    ) -> Option<usize> {
        if host_count == 0 {
            self.next = 0;
            return None;
        }

        let start = self.next % host_count;
        for offset in 0..host_count {
            let index = (start + offset) % host_count;
            if claim(index) {
                self.next = (index + 1) % host_count;
                return Some(index);
            }
        }
        None
    }
}

/// Exponential retry delay capped to keep recovery responsive and finite.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RefreshRetryBackoff {
    next: Duration,
}

impl RefreshRetryBackoff {
    pub(crate) const MIN_DELAY: Duration = Duration::from_millis(1);
    pub(crate) const MAX_DELAY: Duration = Duration::from_millis(100);

    /// Returns this retry's delay and advances to the next bounded delay.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(Self::MAX_DELAY);
        delay
    }

    /// Restores the minimum delay after useful progress or an idle epoch.
    pub(crate) fn reset(&mut self) {
        self.next = Self::MIN_DELAY;
    }
}

impl Default for RefreshRetryBackoff {
    fn default() -> Self {
        Self {
            next: Self::MIN_DELAY,
        }
    }
}

/// Task-context ownership of one host's topology probe.
///
/// IRQ producers publish only the host's atomic dirty bit. The sole USBFS
/// refresh worker folds that bit into this state before and after probing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum HostRefreshState {
    /// The published device snapshot is current.
    #[default]
    Idle,
    /// The refresh worker must probe this host.
    Queued,
    /// The refresh worker exclusively owns an in-flight probe.
    Probing,
    /// A topology event arrived while the refresh worker was probing.
    DirtyAgain,
    /// The backing host permanently disappeared from the device registry.
    Disabled,
}

impl HostRefreshState {
    /// Coalesces one topology change without creating another probe owner.
    pub(crate) fn mark_dirty(&mut self) {
        *self = match *self {
            Self::Idle => Self::Queued,
            Self::Queued => Self::Queued,
            Self::Probing => Self::DirtyAgain,
            Self::DirtyAgain => Self::DirtyAgain,
            Self::Disabled => Self::Disabled,
        };
    }

    /// Transfers a queued probe to the sole refresh worker.
    pub(crate) fn begin_probe(&mut self) -> bool {
        if *self != Self::Queued {
            return false;
        }
        *self = Self::Probing;
        true
    }

    /// Defers an unstarted probe without dropping its dirty state.
    pub(crate) fn defer_probe(&mut self) {
        *self = match *self {
            Self::Probing | Self::DirtyAgain => Self::Queued,
            state => state,
        };
    }

    /// Publishes probe completion and reports whether another pass is required.
    pub(crate) fn finish_probe(&mut self) -> bool {
        match *self {
            Self::Probing => {
                *self = Self::Idle;
                false
            }
            Self::DirtyAgain => {
                *self = Self::Queued;
                true
            }
            Self::Idle | Self::Queued | Self::Disabled => false,
        }
    }

    /// Completes the boot-time probe after its unconditional queued state.
    ///
    /// A topology notification observed during that probe still schedules a
    /// runtime pass instead of being cleared with the bootstrap request.
    pub(crate) fn finish_initial_probe(&mut self) -> bool {
        self.finish_probe()
    }

    /// Returns whether a non-open host can be selected by the worker.
    pub(crate) const fn is_queued(self) -> bool {
        matches!(self, Self::Queued)
    }

    /// Returns whether this host remains visible to USBFS.
    pub(crate) const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Permanently removes this host from refresh selection.
    pub(crate) fn disable(&mut self) {
        *self = Self::Disabled;
    }
}
