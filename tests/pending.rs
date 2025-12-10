use starry_signal::{PendingSignals, SignalInfo, SignalSet, Signo};

#[test]
fn standard_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9);
    assert!(ps.put_signal(sig1.clone()));
    assert!(!ps.put_signal(sig1));
    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    let sig3 = SignalInfo::new_user(Signo::SIGHUP, 9, 9);

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

    let sig4 = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    let sig5 = SignalInfo::new_user(Signo::SIGQUIT, 9, 9);
    assert!(ps.put_signal(sig4));
    assert!(ps.put_signal(sig5));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGTERM);
    assert!(ps.set.has(Signo::SIGQUIT));
}

#[test]
fn realtime_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGRT1, 9, 9);
    let sig2 = SignalInfo::new_user(Signo::SIGRT3, 9, 9);
    let sig3 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9);
    let sig4 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9);

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

    let sig5 = SignalInfo::new_user(Signo::SIGRT3, 9, 9);
    let sig6 = SignalInfo::new_user(Signo::SIGRT2, 9, 9);
    assert!(ps.put_signal(sig5));
    assert!(ps.put_signal(sig6));
    assert_eq!(ps.dequeue_signal(&mask).unwrap().signo(), Signo::SIGRT3);
    assert!(ps.set.has(Signo::SIGRT2));
}

#[test]
fn mixed_signal() {
    let mut ps = PendingSignals::default();
    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9);
    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    let sig3 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9);
    let sig4 = SignalInfo::new_user(Signo::SIGRTMIN, 9, 9);

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
