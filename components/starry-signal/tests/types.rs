use starry_signal::{SignalInfo, SignalSet, Signo};

#[test]
fn signalset_add_remove_has_is_empty() {
    let mut set = SignalSet::default();
    assert!(set.is_empty());

    assert!(set.add(Signo::SIGINT));
    assert!(!set.is_empty());
    assert!(set.has(Signo::SIGINT));

    assert!(!set.add(Signo::SIGINT));

    assert!(set.remove(Signo::SIGINT));
    assert!(!set.has(Signo::SIGINT));
    assert!(set.is_empty());

    assert!(!set.remove(Signo::SIGINT));
}

#[test]
fn signalset_dequeue() {
    let mut set = SignalSet::default();
    assert!(set.add(Signo::SIGTERM));
    assert!(set.add(Signo::SIGINT));
    assert!(set.add(Signo::SIGHUP));

    let mut mask = SignalSet::default();
    mask.add(Signo::SIGHUP);
    mask.add(Signo::SIGINT);
    mask.add(Signo::SIGTERM);

    assert_eq!(set.dequeue(&mask).unwrap(), Signo::SIGHUP);
    assert_eq!(set.dequeue(&mask).unwrap(), Signo::SIGINT);
    assert_eq!(set.dequeue(&mask).unwrap(), Signo::SIGTERM);
    assert!(set.dequeue(&mask).is_none());

    assert!(set.add(Signo::SIGHUP));
    assert!(set.add(Signo::SIGINT));

    let mut mask2 = SignalSet::default();
    mask2.add(Signo::SIGINT);

    assert_eq!(set.dequeue(&mask2).unwrap(), Signo::SIGINT);
    assert!(set.has(Signo::SIGHUP));
}

#[test]
fn signalset_bounds() {
    let mut set = SignalSet::default();
    assert!(set.add(Signo::SIGHUP));
    assert!(set.add(Signo::SIGRT32));
    assert!(set.has(Signo::SIGHUP));
    assert!(set.has(Signo::SIGRT32));
    assert!(set.remove(Signo::SIGHUP));
    assert!(set.remove(Signo::SIGRT32));
}

#[test]
fn signalinfo_new_kernel() {
    let si = SignalInfo::new_kernel(Signo::SIGTERM);
    assert_eq!(si.signo(), Signo::SIGTERM);
    assert_eq!(si.code(), 128);
    assert_eq!(si.errno(), 0);
}

#[test]
fn signalinfo_new_user() {
    let si = SignalInfo::new_user(Signo::SIGINT, 9, 9);
    assert_eq!(si.signo(), Signo::SIGINT);
    assert_eq!(si.code(), 9);
    assert_eq!(
        unsafe {
            si.0.__bindgen_anon_1
                .__bindgen_anon_1
                ._sifields
                ._sigchld
                ._pid
        },
        9
    );
    assert_eq!(si.errno(), 0);
}
