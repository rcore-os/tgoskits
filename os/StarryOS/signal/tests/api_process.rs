use std::sync::Arc;

use ax_runtime::task::sync::RawSpinLock;
use starry_signal::{
    SignalActionFlags, SignalDisposition, SignalInfo, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};

struct TestEnv {
    proc: Arc<ProcessSignalManager>,
}

impl TestEnv {
    fn new() -> Self {
        let actions = Arc::new(RawSpinLock::new(SignalActions::default()));
        let proc = Arc::new(ProcessSignalManager::new(actions, 0, Arc::default()));
        TestEnv { proc }
    }
}

#[test]
fn send_wakes_sets_pending() {
    let env = TestEnv::new();
    let _thr = ThreadSignalManager::new(9, env.proc.clone()).unwrap();
    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 100, 0);

    assert_eq!(env.proc.send_signal(sig.clone(), false), Some(9));
    assert!(env.proc.pending().has(Signo::SIGTERM));
}

#[test]
fn signal_ignore() {
    let env = TestEnv::new();
    env.proc.actions().lock_irqsave()[Signo::SIGTERM].disposition = SignalDisposition::Ignore;
    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 100, 0);

    assert_eq!(env.proc.send_signal(sig, false), None);
    assert!(!env.proc.pending().has(Signo::SIGTERM));
}

#[test]
fn signal_default_ignore() {
    let env = TestEnv::new();
    let sig = SignalInfo::new_user(Signo::SIGCHLD, 0, 100, 0);

    assert_eq!(env.proc.send_signal(sig, false), None);
    assert!(!env.proc.pending().has(Signo::SIGCHLD));
}

#[test]
fn can_restart() {
    let env = TestEnv::new();
    assert!(!env.proc.actions().lock_irqsave()[Signo::SIGTERM].is_restartable());

    env.proc.actions().lock_irqsave()[Signo::SIGTERM]
        .flags
        .insert(SignalActionFlags::RESTART);
    assert!(env.proc.actions().lock_irqsave()[Signo::SIGTERM].is_restartable());
}

#[test]
fn fatal_signal_selects_a_live_unblocked_receiver() {
    let env = TestEnv::new();
    let exiting = ThreadSignalManager::new(1, env.proc.clone()).unwrap();
    let live = ThreadSignalManager::new(2, env.proc.clone()).unwrap();
    assert!(exiting.begin_exit());
    assert!(!exiting.begin_exit());
    let mut blocked = starry_signal::SignalSet::default();
    blocked.add(Signo::SIGTERM);
    live.set_blocked(blocked);
    let signal = SignalInfo::new_kernel(Signo::SIGTERM);
    assert_eq!(env.proc.send_signal(signal, false), None);
    assert_eq!(env.proc.group_exit_status(), None);

    live.set_blocked(starry_signal::SignalSet::default());
    assert_eq!(env.proc.send_signal(signal, false), Some(2));
    assert_eq!(env.proc.group_exit_status(), Some(Signo::SIGTERM as i32));
    assert!(live.pending().has(Signo::SIGKILL));
    assert!(exiting.pending().has(Signo::SIGKILL));

    // A receiver prepared concurrently with group exit inherits the kill bit;
    // clone still rejects its Linux identity publication at the common gate.
    let prepared = ThreadSignalManager::new(3, env.proc.clone()).unwrap();
    assert!(prepared.pending().has(Signo::SIGKILL));
}

#[test]
fn exit_group_publishes_all_peer_kills_before_notification() {
    let env = TestEnv::new();
    let caller = ThreadSignalManager::new(1, env.proc.clone()).unwrap();
    let peer = ThreadSignalManager::new(2, env.proc.clone()).unwrap();
    let exiting_peer = ThreadSignalManager::new(3, env.proc.clone()).unwrap();
    assert!(exiting_peer.begin_exit());

    // No task wake has run yet. Clone's publication gate can now be released
    // only because every potential parent already observes its fatal signal.
    assert_eq!(caller.begin_group_exit(23 << 8), 23 << 8);
    assert!(caller.begin_exit());
    assert_eq!(env.proc.group_exit_status(), Some(23 << 8));
    assert!(!caller.pending().has(Signo::SIGKILL));
    assert!(peer.pending().has(Signo::SIGKILL));
    assert!(exiting_peer.pending().has(Signo::SIGKILL));

    assert_eq!(peer.begin_group_exit(42 << 8), 23 << 8);
    assert_eq!(env.proc.group_exit_status(), Some(23 << 8));
    assert!(!caller.pending().has(Signo::SIGKILL));
    let prepared = ThreadSignalManager::new(4, env.proc.clone()).unwrap();
    assert!(prepared.pending().has(Signo::SIGKILL));
}

#[test]
fn fatal_promotion_excludes_handlers_coredumps_and_deferred_targets() {
    unsafe extern "C" fn receiver(_: i32) {}
    let cases = [
        (Signo::SIGTERM, false, SignalDisposition::Handler(receiver)),
        (Signo::SIGSEGV, false, SignalDisposition::Default),
        (Signo::SIGTERM, true, SignalDisposition::Default),
    ];
    for (signo, defer_fatal, disposition) in cases {
        let env = TestEnv::new();
        let thread = ThreadSignalManager::new(1, env.proc.clone()).unwrap();
        env.proc.actions().lock_irqsave()[signo].disposition = disposition;
        assert_eq!(
            env.proc
                .send_signal(SignalInfo::new_kernel(signo), defer_fatal),
            Some(1)
        );
        assert_eq!(env.proc.group_exit_status(), None);
        assert!(!thread.pending().has(Signo::SIGKILL));
    }
}
