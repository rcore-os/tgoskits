use std::sync::Arc;

use ax_kspin::SpinNoIrq;
use starry_signal::{
    SignalActionFlags, SignalDisposition, SignalInfo, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};

struct TestEnv {
    proc: Arc<ProcessSignalManager>,
}

impl TestEnv {
    fn new() -> Self {
        let actions = Arc::new(SpinNoIrq::new(SignalActions::default()));
        let proc = Arc::new(ProcessSignalManager::new(actions, 0));
        TestEnv { proc }
    }
}

#[test]
fn send_wakes_sets_pending() {
    let env = TestEnv::new();
    let _thr = ThreadSignalManager::new(9, env.proc.clone());
    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 100);

    assert_eq!(env.proc.send_signal(sig.clone()), Some(9));
    assert!(env.proc.pending().has(Signo::SIGTERM));
}

#[test]
fn signal_ignore() {
    let env = TestEnv::new();
    env.proc.actions.lock()[Signo::SIGTERM].disposition = SignalDisposition::Ignore;
    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 100);

    assert_eq!(env.proc.send_signal(sig), None);
    assert!(!env.proc.pending().has(Signo::SIGTERM));
}

#[test]
fn signal_default_ignore() {
    let env = TestEnv::new();
    let sig = SignalInfo::new_user(Signo::SIGCHLD, 0, 100);

    assert_eq!(env.proc.send_signal(sig), None);
    assert!(!env.proc.pending().has(Signo::SIGCHLD));
}

#[test]
fn can_restart() {
    let env = TestEnv::new();
    assert!(!env.proc.actions.lock()[Signo::SIGTERM].is_restartable());

    env.proc.actions.lock()[Signo::SIGTERM]
        .flags
        .insert(SignalActionFlags::RESTART);
    assert!(env.proc.actions.lock()[Signo::SIGTERM].is_restartable());
}
