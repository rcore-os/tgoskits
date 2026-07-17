//! Explicit `LockRuntime` provider for host-side consumer test binaries.
//!
//! This crate is deliberately not a feature of `ax-kspin`: Cargo feature
//! unification must never replace the final kernel's runtime boundary with a
//! host implementation. Each test binary that needs context-aware locks links
//! this fixture explicitly, while binaries with their own provider omit it.

#![no_std]

extern crate std;

use core::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_kspin::{LockRuntime, LockdepEvent, impl_trait};

/// Observable context state for the calling host test thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostContextSnapshot {
    /// Stable nonzero identifier allocated for this host thread.
    pub thread_id: u64,
    /// Number of active IRQ-disabled context entries.
    pub irq_depth: u32,
    /// Number of active preemption-disabled context entries.
    pub preempt_depth: u32,
}

/// Returns the lock-runtime state associated with the calling host thread.
///
/// The snapshot is intended for consumer tests that need to verify context
/// transitions without making `ax-kspin` select a default host provider.
pub fn snapshot_current_thread() -> HostContextSnapshot {
    let thread_id = current_host_thread_id();
    HOST_CONTEXT_DEPTHS.with(|depths| {
        let depths = depths.get();
        HostContextSnapshot {
            thread_id,
            irq_depth: depths.irq,
            preempt_depth: depths.preempt,
        }
    })
}

/// Resets IRQ and preemption depth for the calling host test thread.
///
/// This does not replace the thread's stable identifier. Consumer tests should
/// call it only when no context guard is alive; a later drop of a guard that
/// predates the reset is rejected as a category-specific underflow.
pub fn reset_current_thread_context() {
    HOST_CONTEXT_DEPTHS.with(|depths| depths.set(ContextDepths::default()));
}

struct HostTestLockRuntime;

impl_trait! {
    impl LockRuntime for HostTestLockRuntime {
        fn irq_enter() {
            update_context_depths(ContextDepths::enter_irq);
        }

        fn irq_exit() {
            update_context_depths(ContextDepths::exit_irq);
        }

        fn preempt_enter() {
            update_context_depths(ContextDepths::enter_preempt);
        }

        fn preempt_exit() {
            update_context_depths(ContextDepths::exit_preempt);
        }

        unsafe fn preempt_exit_irq_return() {
            update_context_depths(ContextDepths::exit_preempt);
        }

        fn current_thread_id() -> u64 {
            current_host_thread_id()
        }

        fn lockdep_acquire(_event: LockdepEvent) {}
        fn lockdep_release(_event: LockdepEvent) {}
        fn lockdep_set_trace_enabled(_enabled: bool) {}
        fn lockdep_dump_trace() {}
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ContextDepths {
    irq: u32,
    preempt: u32,
}

impl ContextDepths {
    fn enter_irq(&mut self) {
        self.irq = self
            .irq
            .checked_add(1)
            .expect("host lock runtime IRQ nesting overflow");
    }

    fn exit_irq(&mut self) {
        self.irq = self
            .irq
            .checked_sub(1)
            .expect("host lock runtime IRQ nesting underflow");
    }

    fn enter_preempt(&mut self) {
        self.preempt = self
            .preempt
            .checked_add(1)
            .expect("host lock runtime preemption nesting overflow");
    }

    fn exit_preempt(&mut self) {
        self.preempt = self
            .preempt
            .checked_sub(1)
            .expect("host lock runtime preemption nesting underflow");
    }
}

fn update_context_depths(transition: fn(&mut ContextDepths)) {
    HOST_CONTEXT_DEPTHS.with(|depths| {
        let mut next = depths.get();
        transition(&mut next);
        // Publish only a completed transition. If checked arithmetic rejects
        // the operation, unwinding leaves the prior diagnostic state intact.
        depths.set(next);
    });
}

fn current_host_thread_id() -> u64 {
    HOST_THREAD_ID.with(|thread_id| *thread_id)
}

fn allocate_host_thread_id() -> u64 {
    NEXT_HOST_THREAD_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |thread_id| {
            thread_id.checked_add(1)
        })
        .expect("host lock runtime exhausted thread identifiers")
}

static NEXT_HOST_THREAD_ID: AtomicU64 = AtomicU64::new(1);

std::thread_local! {
    static HOST_CONTEXT_DEPTHS: Cell<ContextDepths> = const {
        Cell::new(ContextDepths { irq: 0, preempt: 0 })
    };
    static HOST_THREAD_ID: u64 = allocate_host_thread_id();
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn irq_depth_rejects_underflow_without_wrapping() {
        let mut depths = ContextDepths::default();

        let result = catch_unwind(AssertUnwindSafe(|| depths.exit_irq()));

        assert!(result.is_err());
        assert_eq!(depths.irq, 0);
        assert_eq!(depths.preempt, 0);
    }

    #[test]
    fn irq_depth_rejects_overflow_without_wrapping() {
        let mut depths = ContextDepths {
            irq: u32::MAX,
            preempt: 0,
        };

        let result = catch_unwind(AssertUnwindSafe(|| depths.enter_irq()));

        assert!(result.is_err());
        assert_eq!(depths.irq, u32::MAX);
        assert_eq!(depths.preempt, 0);
    }

    #[test]
    fn preempt_depth_rejects_underflow_without_wrapping() {
        let mut depths = ContextDepths::default();

        let result = catch_unwind(AssertUnwindSafe(|| depths.exit_preempt()));

        assert!(result.is_err());
        assert_eq!(depths.irq, 0);
        assert_eq!(depths.preempt, 0);
    }

    #[test]
    fn preempt_depth_rejects_overflow_without_wrapping() {
        let mut depths = ContextDepths {
            irq: 0,
            preempt: u32::MAX,
        };

        let result = catch_unwind(AssertUnwindSafe(|| depths.enter_preempt()));

        assert!(result.is_err());
        assert_eq!(depths.irq, 0);
        assert_eq!(depths.preempt, u32::MAX);
    }
}
