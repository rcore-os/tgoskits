//! Component-level heap/lock ownership audit, without emulating IRQ delivery.

extern crate std;

use alloc::sync::{Arc, Weak};
use core::cell::{Cell, RefCell};
use std::alloc::{GlobalAlloc, Layout, System};

use ax_runtime::task::sync::SpinLock;

use crate::api::SignalActions;

std::thread_local! {
    static ACTIONS: RefCell<Weak<SpinLock<SignalActions>>> = const { RefCell::new(Weak::new()) };
    static LOCKED_HEAP_OPERATIONS: Cell<usize> = const { Cell::new(0) };
}

#[global_allocator]
static ALLOCATOR: AuditAllocator = AuditAllocator;

struct AuditAllocator;

// SAFETY: every operation forwards the original pointer and layout unchanged
// to System. The observer only reads the test's retained lock and TLS counters.
unsafe impl GlobalAlloc for AuditAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_heap_operation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_heap_operation();
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_heap_operation();
        unsafe { System.realloc(pointer, layout, size) }
    }
}

fn record_heap_operation() {
    let _ = ACTIONS.try_with(|actions| {
        let observed = actions
            .try_borrow()
            .ok()
            .and_then(|actions| actions.upgrade());
        if observed.is_some_and(|actions| actions.is_locked()) {
            let _ = LOCKED_HEAP_OPERATIONS.try_with(|count| count.set(count.get() + 1));
        }
    });
}

struct AuditScope<'a> {
    // Retain a strong owner for every temporary Weak upgrade in the observer.
    _owner: &'a Arc<SpinLock<SignalActions>>,
}

impl Drop for AuditScope<'_> {
    fn drop(&mut self) {
        ACTIONS.with(|actions| {
            actions.replace(Weak::new());
        });
    }
}

pub(crate) fn with_action_lock<T>(
    actions: &Arc<SpinLock<SignalActions>>,
    operation: impl FnOnce() -> T,
) -> (T, usize) {
    ACTIONS.with(|observed| {
        assert!(
            observed.borrow().upgrade().is_none(),
            "nested allocation audit"
        );
        observed.replace(Arc::downgrade(actions));
    });
    LOCKED_HEAP_OPERATIONS.with(|count| count.set(0));
    let scope = AuditScope { _owner: actions };
    let result = operation();
    drop(scope);
    (result, LOCKED_HEAP_OPERATIONS.with(Cell::get))
}
