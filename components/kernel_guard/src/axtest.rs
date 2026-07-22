use ax_kernel_guard::{BaseGuard, NoOp, NoPreempt};
use axtest::prelude::*;

use crate as ax_kernel_guard;

#[axtest::def_test]
fn kernel_guard_noop_contracts_hold() {
    <NoOp as BaseGuard>::acquire();
    <NoOp as BaseGuard>::release(());
    ax_assert!(!<NoOp as BaseGuard>::lockdep_enabled());

    let guard = NoOp::new();
    drop(guard);

    let default_guard = NoOp::default();
    drop(default_guard);
}

#[axtest::def_test]
fn kernel_guard_no_preempt_contracts_hold() {
    <NoPreempt as BaseGuard>::acquire();
    <NoPreempt as BaseGuard>::release(());
    ax_assert!(<NoPreempt as BaseGuard>::lockdep_enabled());

    let guard = NoPreempt::new();
    drop(guard);

    let default_guard = NoPreempt::default();
    drop(default_guard);
}
