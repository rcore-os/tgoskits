use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use axtest::prelude::*;
use scope_local::{ActiveScope, Scope, scope_local};

use crate as scope_local;

static UNUSED_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

scope_local! {
    static COVERAGE_NUMBER: usize = 7;
    static COVERAGE_SHARED: Arc<()> = Arc::new(());
    static COVERAGE_UNUSED: usize = {
        UNUSED_INIT_COUNT.fetch_add(1, Ordering::AcqRel);
        11
    };
}

#[axtest]
fn scope_local_explicit_scope_values_are_lazy_and_isolated() {
    UNUSED_INIT_COUNT.store(0, Ordering::Release);

    let mut first_scope = Scope::new();
    let mut second_scope = Scope::default();

    ax_assert_eq!(*COVERAGE_NUMBER.scope(&first_scope), 7);
    ax_assert_eq!(UNUSED_INIT_COUNT.load(Ordering::Acquire), 0);

    *COVERAGE_NUMBER.scope_mut(&mut first_scope) = 41;
    *COVERAGE_NUMBER.scope_mut(&mut second_scope) = 99;

    ax_assert_eq!(*COVERAGE_NUMBER.scope(&first_scope), 41);
    ax_assert_eq!(*COVERAGE_NUMBER.scope(&second_scope), 99);
    ax_assert_eq!(*COVERAGE_NUMBER, 7);

    ax_assert_eq!(*COVERAGE_UNUSED.scope(&first_scope), 11);
    ax_assert_eq!(UNUSED_INIT_COUNT.load(Ordering::Acquire), 1);
}

#[axtest]
fn scope_local_drops_scope_owned_values() {
    ax_assert_eq!(Arc::strong_count(&COVERAGE_SHARED), 1);

    {
        let mut scope = Scope::new();
        *COVERAGE_SHARED.scope_mut(&mut scope) = COVERAGE_SHARED.clone();

        ax_assert_eq!(Arc::strong_count(&COVERAGE_SHARED), 2);
        ax_assert!(Arc::ptr_eq(
            &COVERAGE_SHARED,
            &COVERAGE_SHARED.scope(&scope)
        ));
    }

    ax_assert_eq!(Arc::strong_count(&COVERAGE_SHARED), 1);
}

#[axtest]
fn scope_local_active_scope_switching_rules_hold() {
    ActiveScope::set_global();
    ax_assert!(ActiveScope::is_global());
    ax_assert_eq!(*COVERAGE_NUMBER, 7);

    let mut scope = Scope::new();
    *COVERAGE_NUMBER.scope_mut(&mut scope) = 123;

    unsafe { ActiveScope::set(&scope) };
    ax_assert!(!ActiveScope::is_global());
    ax_assert_eq!(*COVERAGE_NUMBER, 123);

    ActiveScope::set_global();
    ax_assert!(ActiveScope::is_global());
    ax_assert_eq!(*COVERAGE_NUMBER, 7);
}

#[axtest]
fn scope_local_boxed_layout_rules_hold() {
    ax_assert!(crate::boxed::boxed_layout_rules_hold_for_test());
}

#[axtest]
fn scope_local_boxed_layout_more_edge_cases_hold() {
    ax_assert!(crate::boxed::boxed_layout_more_edge_cases_hold_for_test());
}

#[axtest]
fn scope_local_boxed_layout_comprehensive_hold() {
    ax_assert!(crate::boxed::boxed_layout_comprehensive_hold_for_test());
}

#[axtest]
fn scope_local_boxed_header_size_and_alignment_hold() {
    ax_assert!(crate::boxed::boxed_header_size_and_alignment_hold_for_test());
}

#[axtest]
fn scope_local_boxed_layout_alignment_edge_cases_hold() {
    ax_assert!(crate::boxed::boxed_layout_alignment_edge_cases_hold_for_test());
}
