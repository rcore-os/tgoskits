use linux_raw_sys::general::kernel_sigaction;
use starry_signal::{SignalAction, SignalActionFlags, SignalDisposition, SignalSet, Signo};

#[test]
fn flags_bits() {
    let mut flags = SignalActionFlags::default();
    flags.insert(SignalActionFlags::SIGINFO);
    assert!(flags.contains(SignalActionFlags::SIGINFO));

    flags.insert(SignalActionFlags::ONSTACK);
    assert!(flags.contains(SignalActionFlags::ONSTACK));

    flags.remove(SignalActionFlags::SIGINFO);
    assert!(!flags.contains(SignalActionFlags::SIGINFO));
    assert!(flags.contains(SignalActionFlags::ONSTACK));

    let bits = flags.bits();
    assert_ne!(bits, 0);
    assert!(!flags.is_empty());
}

#[test]
fn convert() {
    unsafe extern "C" fn test_handler(_: i32) {}
    let flag_disposition = vec![
        (SignalActionFlags::empty(), SignalDisposition::Default),
        (
            SignalActionFlags::RESTART | SignalActionFlags::ONSTACK,
            SignalDisposition::Ignore,
        ),
        (
            SignalActionFlags::SIGINFO | SignalActionFlags::NODEFER,
            SignalDisposition::Handler(test_handler),
        ),
    ];

    for (flags, disposition) in flag_disposition {
        let action = SignalAction {
            flags,
            mask: {
                let mut m = SignalSet::default();
                m.add(Signo::SIGINT);
                m.add(Signo::SIGRT32);
                m
            },
            disposition: disposition.clone(),
            restorer: None,
        };
        let ks: kernel_sigaction = action.clone().into();
        let action2 = SignalAction::from(ks);

        assert_eq!(action.flags.bits(), action2.flags.bits());
        assert_eq!(
            action.mask.has(Signo::SIGINT),
            action2.mask.has(Signo::SIGINT)
        );
        assert_eq!(
            action.mask.has(Signo::SIGRT32),
            action2.mask.has(Signo::SIGRT32)
        );
        match (&action.disposition, &action2.disposition) {
            (SignalDisposition::Default, SignalDisposition::Default) => {}
            (SignalDisposition::Ignore, SignalDisposition::Ignore) => {}
            (SignalDisposition::Handler(h1), SignalDisposition::Handler(h2)) => {
                let p1 = *h1 as usize;
                let p2 = *h2 as usize;
                assert_ne!(p1, 0);
                assert_eq!(p1, p2);
            }
            _ => panic!(
                "Unexpected disposition combination: {:?} -> {:?}",
                action.disposition, action2.disposition
            ),
        }
    }
}
