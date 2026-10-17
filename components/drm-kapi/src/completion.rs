// SPDX-License-Identifier: MIT
//! One-shot style completion events with a counting model.
//!
//! `complete()` adds one unit of credit and each successful wait that is not
//! the saturated "complete all" state consumes one unit. `complete_all()` sets
//! the credit to `UINT_MAX`, which every waiter may consume without limit.
//! Waiters poll the credit and yield between checks rather than parking on a
//! wait queue, so a wake-up is seen within one scheduler yield.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    clock,
    config::{CONFIG_HZ, ERESTARTSYS, MAX_SCHEDULE_TIMEOUT},
};

/// Saturated credit value written by `complete_all()`.
pub const UINT_MAX: u32 = u32::MAX;

/// Completion event. The x86_64 layout is fixed because DRM ports embed it in
/// C-shaped structures: the credit counter sits at offset 0, followed by a
/// 24-byte block reserved for a wait-queue head. Size 32, alignment 8.
#[repr(C, align(8))]
pub struct Completion {
    done: AtomicU32,
    _pad: u32,
    /// Reserved wait-queue storage. Waiters poll `done` instead, so the bytes
    /// are only kept to preserve the layout.
    _swait: [u8; 24],
}

const _: [(); 32] = [(); core::mem::size_of::<Completion>()];
const _: [(); 8] = [(); core::mem::offset_of!(Completion, _swait)];

/// Const constructor for a completion with no credit.
pub const fn init_completion() -> Completion {
    Completion {
        done: AtomicU32::new(0),
        _pad: 0,
        _swait: [0; 24],
    }
}

/// How long a wait may last, in nanoseconds of the monotonic clock.
#[derive(Clone, Copy)]
enum Budget {
    Unbounded,
    Ns(u64),
}

impl Budget {
    fn from_jiffies(jiffies: i64) -> Self {
        if jiffies >= MAX_SCHEDULE_TIMEOUT as i64 {
            return Budget::Unbounded;
        }
        let ns_per_tick = 1_000_000_000 / u64::from(CONFIG_HZ);
        let ticks = u64::try_from(jiffies).unwrap_or(0);
        Budget::Ns(ticks.saturating_mul(ns_per_tick))
    }

    /// Whole jiffies left after a successful wait that took `elapsed_ns`.
    /// Rounds up and never reports less than one.
    fn remaining_jiffies(self, elapsed_ns: u64) -> i64 {
        match self {
            Budget::Unbounded => MAX_SCHEDULE_TIMEOUT as i64,
            Budget::Ns(total) => {
                let ns_per_tick = 1_000_000_000 / u64::from(CONFIG_HZ);
                let left = total.saturating_sub(elapsed_ns);
                (left.div_ceil(ns_per_tick) as i64).max(1)
            }
        }
    }

    fn is_exhausted(self, elapsed_ns: u64) -> bool {
        match self {
            Budget::Unbounded => false,
            Budget::Ns(total) => elapsed_ns >= total,
        }
    }
}

impl Completion {
    /// A completion with no credit, usable in `static` items.
    pub const fn new() -> Self {
        init_completion()
    }

    /// Discards any credit that was not consumed.
    pub fn reinit(&self) {
        self.done.store(0, Ordering::Release);
    }

    /// Adds one unit of credit. Credit stops growing at `UINT_MAX`.
    pub fn complete(&self) {
        let _ = self
            .done
            .try_update(Ordering::AcqRel, Ordering::Acquire, |credit| {
                credit.checked_add(1)
            });
    }

    /// Sets the credit to `UINT_MAX`, releasing every current and future waiter.
    pub fn complete_all(&self) {
        self.done.store(UINT_MAX, Ordering::Release);
    }

    /// Takes one unit of credit without blocking. Returns whether one was
    /// available.
    pub fn try_wait(&self) -> bool {
        self.done
            .try_update(Ordering::AcqRel, Ordering::Acquire, |credit| match credit {
                0 => None,
                UINT_MAX => Some(UINT_MAX),
                _ => Some(credit - 1),
            })
            .is_ok()
    }

    /// Reports whether a waiter would currently proceed without blocking.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire) != 0
    }

    /// Blocks until credit is available. Not interruptible.
    pub fn wait(&self) {
        let _ = self.wait_for(MAX_SCHEDULE_TIMEOUT as i64, false);
    }

    /// Blocks for at most `timeout_jiffies`. Returns the jiffies left (at
    /// least one) if the credit was taken, or zero if the time ran out.
    pub fn wait_timeout(&self, timeout_jiffies: i64) -> i64 {
        self.wait_for(timeout_jiffies, false)
    }

    /// Blocks until credit is available or a signal arrives. Returns zero on
    /// success or `-ERESTARTSYS` when interrupted.
    pub fn wait_interruptible(&self) -> i32 {
        if self.wait_for(MAX_SCHEDULE_TIMEOUT as i64, true) == -(ERESTARTSYS as i64) {
            -ERESTARTSYS
        } else {
            0
        }
    }

    /// Like [`Completion::wait_timeout`], but also returns early with
    /// `-ERESTARTSYS` when a signal is pending.
    pub fn wait_interruptible_timeout(&self, timeout_jiffies: i64) -> i64 {
        self.wait_for(timeout_jiffies, true)
    }

    /// Polls for credit until it is taken, the budget runs out, or (when
    /// `interruptible`) a signal is pending. Credit is checked before any of
    /// the other conditions, so a waiter that already has credit succeeds.
    fn wait_for(&self, timeout_jiffies: i64, interruptible: bool) -> i64 {
        let budget = Budget::from_jiffies(timeout_jiffies);
        let started = clock::now_ns();
        loop {
            if self.try_wait() {
                let elapsed = clock::now_ns().saturating_sub(started);
                return budget.remaining_jiffies(elapsed);
            }
            let elapsed = clock::now_ns().saturating_sub(started);
            if interruptible && interrupted() {
                return -(ERESTARTSYS as i64);
            }
            if budget.is_exhausted(elapsed) {
                return 0;
            }
            clock::relax();
        }
    }
}

/// Reports whether the current task has a pending signal. Host tests run
/// without a task runtime, so they never see one.
#[cfg(not(test))]
fn interrupted() -> bool {
    crate::signal::signal_pending_current()
}

#[cfg(test)]
fn interrupted() -> bool {
    false
}

impl Default for Completion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn complete_then_wait_returns_immediately_and_consumes_one_count() {
        let completion = Completion::new();
        assert!(!completion.is_done());
        completion.complete();
        assert!(completion.is_done());
        assert!(completion.wait_timeout(0) >= 1);
        assert!(!completion.is_done());
        assert_eq!(completion.wait_timeout(1), 0);
    }

    #[test]
    fn counts_accumulate_and_try_wait_consumes_one_at_a_time() {
        let completion = Completion::new();
        completion.complete();
        completion.complete();
        assert!(completion.try_wait());
        assert!(completion.try_wait());
        assert!(!completion.try_wait());
    }

    #[test]
    fn complete_all_saturates_and_never_consumes() {
        let completion = Completion::new();
        completion.complete_all();
        completion.complete();
        for _ in 0..3 {
            assert!(completion.try_wait());
            assert!(completion.is_done());
        }
        assert_eq!(completion.done.load(Ordering::Relaxed), UINT_MAX);
    }

    #[test]
    fn reinit_forgets_pending_completions() {
        let completion = Completion::new();
        completion.complete_all();
        completion.reinit();
        assert!(!completion.is_done());
        assert!(!completion.try_wait());
    }

    #[test]
    fn timed_wait_reports_zero_when_nothing_completes() {
        let completion = Completion::new();
        assert_eq!(completion.wait_timeout(1), 0);
        assert_eq!(completion.wait_interruptible_timeout(1), 0);
    }

    #[test]
    fn completion_from_another_thread_wakes_the_waiter() {
        let completion = Arc::new(Completion::new());
        let signaller = Arc::clone(&completion);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            signaller.complete();
        });
        assert!(completion.wait_timeout(500) >= 1);
        handle.join().unwrap();
    }

    #[test]
    fn unbounded_wait_returns_after_complete_all_from_another_thread() {
        let completion = Arc::new(Completion::new());
        let signaller = Arc::clone(&completion);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            signaller.complete_all();
        });
        completion.wait();
        assert_eq!(completion.wait_interruptible(), 0);
        handle.join().unwrap();
    }
}
