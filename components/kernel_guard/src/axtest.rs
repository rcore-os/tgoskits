use ax_kernel_guard::{BaseGuard, IrqSave, NoOp, NoPreempt, NoPreemptIrqSave};
use axtest::prelude::*;

use crate as ax_kernel_guard;

#[axtest]
fn kernel_guard_noop_contracts_hold() {
    <NoOp as BaseGuard>::acquire();
    <NoOp as BaseGuard>::release(());
    ax_assert!(!<NoOp as BaseGuard>::lockdep_enabled());

    let guard = NoOp::new();
    drop(guard);

    let default_guard = NoOp::default();
    drop(default_guard);
}

#[axtest]
fn kernel_guard_no_preempt_contracts_hold() {
    <NoPreempt as BaseGuard>::acquire();
    <NoPreempt as BaseGuard>::release(());
    ax_assert!(<NoPreempt as BaseGuard>::lockdep_enabled());

    let guard = NoPreempt::new();
    drop(guard);

    let default_guard = NoPreempt::default();
    drop(default_guard);
}

#[axtest]
fn kernel_guard_irq_save_contracts_hold() {
    let state = <IrqSave as BaseGuard>::acquire();
    <IrqSave as BaseGuard>::release(state);
    ax_assert!(!<IrqSave as BaseGuard>::lockdep_enabled());

    let guard = IrqSave::new();
    drop(guard);

    let default_guard = IrqSave::default();
    drop(default_guard);
}

#[axtest]
fn kernel_guard_no_preempt_irq_save_contracts_hold() {
    let state = <NoPreemptIrqSave as BaseGuard>::acquire();
    <NoPreemptIrqSave as BaseGuard>::release(state);
    ax_assert!(<NoPreemptIrqSave as BaseGuard>::lockdep_enabled());

    let guard = NoPreemptIrqSave::new();
    drop(guard);

    let default_guard = NoPreemptIrqSave::default();
    drop(default_guard);
}
