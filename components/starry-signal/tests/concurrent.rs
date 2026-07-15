use std::{
    thread,
    time::{Duration, Instant},
};

use ax_cpu::uspace::UserContext;
use ax_kspin_test_runtime as _;
use starry_signal::{SignalDisposition, SignalInfo, SignalOSAction, SignalSet, Signo};

mod common;
use common::*;

fn wait_until<F>(mut check: F) -> bool
where
    F: FnMut() -> bool,
{
    const TIMEOUT: Duration = Duration::from_millis(100);

    let start = Instant::now();
    while start.elapsed() < TIMEOUT {
        if check() {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn concurrent_send_signal() {
    let (proc, thr) = new_test_env();

    let signo = Signo::SIGTERM;
    let sig = SignalInfo::new_user(signo, 9, 9, 0);

    thread::spawn({
        let thr = thr.clone();
        move || {
            thread::sleep(Duration::from_millis(10));
            let _ = thr.send_signal(sig);
        }
    });

    assert!(wait_until(
        || thr.pending().has(signo) || proc.pending().has(signo)
    ));
}

#[test]
fn concurrent_blocked() {
    let (_proc, thr) = new_test_env();

    let signo = Signo::SIGTERM;
    let sig = SignalInfo::new_user(signo, 9, 9, 0);

    let mut blocked = SignalSet::default();
    blocked.add(signo);
    let prev = thr.set_blocked(blocked);
    assert!(!prev.has(signo));
    assert!(thr.signal_blocked(signo));

    thread::spawn({
        let thr = thr.clone();
        move || {
            thread::sleep(Duration::from_millis(10));
            let _ = thr.send_signal(sig);
        }
    });

    assert!(wait_until(|| thr.pending().has(signo)));

    thr.set_blocked(SignalSet::default());
    assert!(!thr.signal_blocked(signo));

    let mut uctx = UserContext::new(0, 0.into(), 0);
    let res = wait_until(|| {
        if let Some((si, _)) = thr.check_signals(&mut uctx, None) {
            assert_eq!(si.signo(), signo);
            true
        } else {
            false
        }
    });
    assert!(res);
}

#[test]
fn concurrent_check_signals() {
    let (proc, thr) = new_test_env();

    unsafe extern "C" fn test_handler(_: i32) {}
    proc.actions().lock()[Signo::SIGTERM].disposition = SignalDisposition::Handler(test_handler);

    let mut uctx = UserContext::new(0, initial_sp().into(), 0);

    let first = SignalInfo::new_user(Signo::SIGTERM, 9, 9, 0);
    assert!(thr.send_signal(first.clone()));

    let (si, action) = thr.check_signals(&mut uctx, None).unwrap();
    assert_eq!(si.signo(), Signo::SIGTERM);
    assert_eq!(action, SignalOSAction::NoFurtherAction);
    assert!(thr.signal_blocked(Signo::SIGTERM));

    thread::spawn({
        let thr = thr.clone();
        move || {
            let _ = thr.send_signal(SignalInfo::new_user(Signo::SIGINT, 2, 2, 0));
            let _ = thr.send_signal(SignalInfo::new_user(Signo::SIGTERM, 3, 3, 0));
        }
    });

    assert!(wait_until(|| thr.pending().has(Signo::SIGTERM)));
    assert!(wait_until(|| thr.pending().has(Signo::SIGINT)));

    prepare_restore_context(&mut uctx);
    thr.restore(&mut uctx).unwrap();

    assert!(!thr.signal_blocked(Signo::SIGTERM));

    let mut delivered = SignalSet::default();
    assert!(wait_until(|| {
        if let Some((sig, _)) = thr.check_signals(&mut uctx, None) {
            delivered.add(sig.signo());
        }
        delivered.has(Signo::SIGINT) && delivered.has(Signo::SIGTERM)
    }));
}
