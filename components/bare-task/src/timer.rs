//! Timer wheel/runtime core for future timers.

use alloc::{collections::BTreeMap, vec::Vec};
use core::task::{Poll, Waker};

/// Timer key returned by [`TimerRuntimeCore::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimerKey {
    deadline_nanos: u64,
    key: u64,
}

impl TimerKey {
    /// Returns the absolute monotonic deadline in nanoseconds.
    pub const fn deadline_nanos(self) -> u64 {
        self.deadline_nanos
    }
}

/// Future timer runtime without OS timer programming.
pub struct TimerRuntimeCore {
    key: u64,
    wheel: BTreeMap<TimerKey, Waker>,
}

impl TimerRuntimeCore {
    /// Creates an empty timer runtime.
    pub const fn new() -> Self {
        Self {
            key: 0,
            wheel: BTreeMap::new(),
        }
    }

    /// Adds a timer if `deadline_nanos` is in the future.
    pub fn add(&mut self, deadline_nanos: u64, now_nanos: u64) -> Option<TimerKey> {
        if deadline_nanos <= now_nanos {
            return None;
        }

        let key = TimerKey {
            deadline_nanos,
            key: self.key,
        };
        self.wheel.insert(key, Waker::noop().clone());
        self.key += 1;
        Some(key)
    }

    /// Polls a timer key.
    pub fn poll(&mut self, key: &TimerKey, waker: &Waker) -> Poll<()> {
        if let Some(slot) = self.wheel.get_mut(key) {
            *slot = waker.clone();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    /// Cancels a timer key.
    pub fn cancel(&mut self, key: &TimerKey) {
        self.wheel.remove(key);
    }

    /// Returns the next deadline in monotonic nanoseconds.
    pub fn next_deadline_nanos(&self) -> Option<u64> {
        self.wheel.keys().next().map(|key| key.deadline_nanos)
    }

    /// Takes all expired timer wakers up to `now_nanos`.
    pub fn take_expired(&mut self, now_nanos: u64) -> Vec<Waker> {
        if self.wheel.is_empty() {
            return Vec::new();
        }

        let pending = self.wheel.split_off(&TimerKey {
            deadline_nanos: now_nanos,
            key: u64::MAX,
        });

        let expired = core::mem::replace(&mut self.wheel, pending);
        expired.into_values().collect()
    }
}

impl Default for TimerRuntimeCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Waker,
    };

    use super::TimerRuntimeCore;

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn timer_runtime_orders_cancels_and_takes_expired() {
        let mut runtime = TimerRuntimeCore::new();
        let count = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let first = runtime.add(10, 0).unwrap();
        let second = runtime.add(20, 0).unwrap();

        assert_eq!(runtime.next_deadline_nanos(), Some(10));
        assert!(runtime.poll(&first, &waker).is_pending());
        runtime.cancel(&second);

        for waker in runtime.take_expired(10) {
            waker.wake();
        }

        assert_eq!(count.0.load(Ordering::Acquire), 1);
        assert_eq!(runtime.next_deadline_nanos(), None);
    }
}
