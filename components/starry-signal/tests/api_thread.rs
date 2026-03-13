use axcpu::uspace::UserContext;
use starry_signal::{SignalDisposition, SignalInfo, SignalOSAction, SignalSet, Signo};

mod common;
use common::*;

#[test]
fn dequeue_signal() {
    let (proc, thr) = new_test_env();

    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9);
    assert!(thr.send_signal(sig1));

    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    assert_eq!(proc.send_signal(sig2), Some(TID));

    let mask = !SignalSet::default();
    assert_eq!(thr.dequeue_signal(&mask).unwrap().signo(), Signo::SIGINT);
    assert_eq!(thr.dequeue_signal(&mask).unwrap().signo(), Signo::SIGTERM);
    assert!(thr.dequeue_signal(&mask).is_none());
}

#[test]
fn handle_signal() {
    let (proc, thr) = new_test_env();

    let signo = Signo::SIGTERM;
    let sig = SignalInfo::new_user(signo, 9, 9);

    unsafe extern "C" fn test_handler(_: i32) {}
    proc.actions.lock()[signo].disposition = SignalDisposition::Handler(test_handler);

    let initial = UserContext::new(0, initial_sp().into(), 0);

    let mut uctx = initial;
    let restore_blocked = thr.blocked();
    let action = proc.actions.lock()[signo].clone();
    let result = thr.handle_signal(&mut uctx, restore_blocked, &sig, &action);

    assert_eq!(result, Some(SignalOSAction::Handler));
    assert_eq!(uctx.ip(), test_handler as *const () as usize);
    assert!(uctx.sp() < initial.sp());
    assert_eq!(uctx.arg0(), signo as usize);
}

#[test]
fn block_ignore_send_signal() {
    let (proc, thr) = new_test_env();

    let signo = Signo::SIGINT;
    let sig = SignalInfo::new_user(signo, 0, 1);
    assert!(thr.send_signal(sig.clone()));
    assert_eq!(
        thr.dequeue_signal(&!SignalSet::default()).unwrap().signo(),
        sig.signo()
    );

    proc.actions.lock()[signo].disposition = SignalDisposition::Ignore;
    assert!(!thr.send_signal(sig.clone()));
    assert!(!thr.pending().has(signo));

    let mut set = SignalSet::default();
    set.add(signo);
    thr.set_blocked(set);
    assert!(thr.signal_blocked(signo));
    assert!(!thr.send_signal(sig.clone()));
    assert!(!thr.pending().has(signo));

    proc.actions.lock()[signo].disposition = SignalDisposition::Default;
    assert!(!thr.send_signal(sig.clone()));
    assert!(thr.pending().has(signo));

    let empty = SignalSet::default();
    thr.set_blocked(empty);
    assert!(!thr.signal_blocked(signo));
}

#[test]
fn check_signals() {
    let (proc, thr) = new_test_env();

    let mut uctx = UserContext::new(0, 0.into(), 0);

    let signo = Signo::SIGTERM;
    let sig = SignalInfo::new_user(signo, 0, 1);

    assert_eq!(proc.send_signal(sig.clone()), Some(TID));
    let (si, _os_action) = thr.check_signals(&mut uctx, None).unwrap();
    assert_eq!(si.signo(), signo);

    assert!(thr.send_signal(sig.clone()));
    let (si, _os_action) = thr.check_signals(&mut uctx, None).unwrap();
    assert_eq!(si.signo(), signo);
}

#[test]
fn restore() {
    let (proc, thr) = new_test_env();

    let signo = Signo::SIGTERM;
    let sig = SignalInfo::new_user(signo, 0, 1);

    unsafe extern "C" fn test_handler(_: i32) {}
    proc.actions.lock()[signo].disposition = SignalDisposition::Handler(test_handler);

    let initial = UserContext::new(0x219, initial_sp().into(), 0);

    let mut uctx = initial;
    let restore_blocked = thr.blocked();
    let action = proc.actions.lock()[sig.signo()].clone();
    thr.handle_signal(&mut uctx, restore_blocked, &sig, &action);

    let new_sp = uctx.sp() + 8;
    uctx.set_sp(new_sp);
    thr.restore(&mut uctx);

    assert_eq!(uctx.ip(), initial.ip());
    assert_eq!(uctx.sp(), initial.sp());
}
