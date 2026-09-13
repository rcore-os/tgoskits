use starry_signal::{PendingSignals, SEGV_MAPERR, SI_USER, SignalInfo, SignalSet, Signo};

#[test]
fn standard_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9, 0);
    assert!(ps.put_signal(sig1.clone()));
    assert!(!ps.put_signal(sig1));
    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9, 0);
    let sig3 = SignalInfo::new_user(Signo::SIGHUP, 9, 9, 0);

    let mut mask = SignalSet::default();
    mask.add(Signo::SIGHUP);
    mask.add(Signo::SIGTERM);
    mask.add(Signo::SIGINT);

    assert!(ps.put_signal(sig3));
    assert!(ps.put_signal(sig2));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGHUP);
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGINT);
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGTERM);
    assert!(ps.dequeue_signal(&mask).is_none());

    let sig4 = SignalInfo::new_user(Signo::SIGTERM, 9, 9, 0);
    let sig5 = SignalInfo::new_user(Signo::SIGQUIT, 9, 9, 0);
    assert!(ps.put_signal(sig4));
    assert!(ps.put_signal(sig5));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGTERM);
    assert!(ps.set.has(Signo::SIGQUIT));
}

#[test]
fn realtime_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGRT1, 9, 9, 0);
    let sig2 = SignalInfo::new_user(Signo::SIGRT3, 9, 9, 0);
    let sig3 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9, 0);
    let sig4 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9, 0);

    let mut mask = SignalSet::default();
    mask.add(Signo::SIGRT3);
    mask.add(Signo::SIGRT1);
    mask.add(Signo::SIGRTMIN);

    assert!(ps.put_signal(sig1));
    assert!(ps.put_signal(sig2));
    assert!(ps.put_signal(sig3));
    assert!(ps.put_signal(sig4));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRTMIN);
    assert!(ps.set.has(Signo::SIGRTMIN));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRTMIN);
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRT1);
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRT3);
    assert!(ps.dequeue_signal(&mask).is_none());

    let sig5 = SignalInfo::new_user(Signo::SIGRT3, 9, 9, 0);
    let sig6 = SignalInfo::new_user(Signo::SIGRT2, 9, 9, 0);
    assert!(ps.put_signal(sig5));
    assert!(ps.put_signal(sig6));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRT3);
    assert!(ps.set.has(Signo::SIGRT2));
}

#[test]
fn mixed_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9, 0);
    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9, 0);
    let sig3 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9, 0);
    let sig4 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9, 0);

    let mut mask = SignalSet::default();
    mask.add(Signo::SIGINT);
    mask.add(Signo::SIGTERM);
    mask.add(Signo::SIGRTMIN);

    assert!(ps.put_signal(sig1));
    assert!(ps.put_signal(sig2));
    assert!(ps.put_signal(sig3));
    assert!(ps.put_signal(sig4));

    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGINT);
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGTERM);

    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRTMIN);
    assert!(ps.set.has(Signo::SIGRTMIN));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRTMIN);
    assert!(ps.dequeue_signal(&mask).is_none());
}

#[test]
fn synchronous_fault_preempts_lower_numbered_async_signal() {
    // A synchronous SIGSEGV (signo 11, si_code = SEGV_MAPERR > SI_USER) pending
    // together with an asynchronous SIGUSR1 (signo 10) must be delivered first,
    // mirroring Linux `dequeue_synchronous_signal`, even though SIGUSR1 is the
    // lower-numbered signal that plain `dequeue_signal` would pick.
    let mut ps = PendingSignals::default();
    assert!(ps.put_signal(SignalInfo::new_user(Signo::SIGUSR1, 0, 9, 0)));
    assert!(ps.put_signal(SignalInfo::new_fault(
        Signo::SIGSEGV,
        SEGV_MAPERR,
        0xdead_beef
    )));

    let all = !SignalSet::default();
    assert_eq!(
        ps.dequeue_synchronous_signal(&all).unwrap().signo(),
        Signo::SIGSEGV
    );
    // Nothing synchronous remains; SIGUSR1 is then taken by normal dequeue.
    assert!(ps.dequeue_synchronous_signal(&all).is_none());
    assert_eq!(ps.dequeue_signal(&all).unwrap().signo(), Signo::SIGUSR1);
    assert!(ps.dequeue_signal(&all).is_none());
}

#[test]
fn user_sent_synchronous_signal_is_not_prioritized() {
    // A user-sent kill(SIGSEGV) carries si_code == SI_USER, so it is NOT an
    // instruction-generated fault and must not preempt a lower-numbered pending
    // signal via the synchronous path (matches Linux `si_code > SI_USER` gate).
    let mut ps = PendingSignals::default();
    assert!(ps.put_signal(SignalInfo::new_user(Signo::SIGHUP, 0, 9, 0)));
    assert!(ps.put_signal(SignalInfo::new_user(Signo::SIGSEGV, SI_USER, 9, 0)));

    let all = !SignalSet::default();
    assert!(ps.dequeue_synchronous_signal(&all).is_none());
    // Normal lowest-numbered ordering: SIGHUP (1) before SIGSEGV (11).
    assert_eq!(ps.dequeue_signal(&all).unwrap().signo(), Signo::SIGHUP);
    assert_eq!(ps.dequeue_signal(&all).unwrap().signo(), Signo::SIGSEGV);
}
