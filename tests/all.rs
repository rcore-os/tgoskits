use axns::{Namespace, def_resource};

#[test]
fn ns() {
    #[derive(Debug)]
    struct Data {
        a: i32,
        b: String,
    }

    def_resource! {
        static DATA: Data = Data { a: 100, b: "hello".to_string() };
    }

    let mut ns = Namespace::new();

    *DATA.get_mut(&mut ns).unwrap() = Data {
        a: 42,
        b: "world".to_string(),
    };

    let res = DATA.current();
    assert_eq!(res.a, 100);
    assert_eq!(res.b, "hello");
}

#[test]
fn current() {
    use std::sync::atomic::{AtomicI32, Ordering::Relaxed};

    def_resource! {
        static DATA: AtomicI32 = AtomicI32::new(100);
    }

    assert_eq!(DATA.current().load(Relaxed), 100);
    DATA.current().store(42, Relaxed);
    assert_eq!(DATA.current().load(Relaxed), 42);
}

#[cfg(feature = "thread-local")]
mod local {
    use std::{sync::Arc, thread};

    use axns::{CurrentNs, Namespace, def_resource, global_ns};
    use extern_trait::extern_trait;
    use parking_lot::{ArcRwLockReadGuard, RawRwLock, RwLock};
    use spin::{Lazy, Once};

    thread_local! {
        static NS: Once<Arc<RwLock<Namespace>>> = const { Once::new() };
    }

    struct CurrentNsImpl(Option<ArcRwLockReadGuard<RawRwLock, Namespace>>);

    impl AsRef<Namespace> for CurrentNsImpl {
        fn as_ref(&self) -> &Namespace {
            if let Some(ns) = &self.0 {
                ns
            } else {
                global_ns()
            }
        }
    }

    #[extern_trait]
    unsafe impl CurrentNs for CurrentNsImpl {
        fn new() -> Self {
            NS.with(|ns| CurrentNsImpl(ns.get().map(RwLock::read_arc)))
        }
    }

    #[test]
    fn recycle() {
        static SHARED: Lazy<Arc<()>> = Lazy::new(|| Arc::new(()));
        def_resource! {
            static DATA: Arc<()> = Arc::new(());
        }

        thread::spawn(|| {
            NS.with(|ns| {
                ns.call_once(|| {
                    let mut ns = Namespace::new();
                    DATA.get_mut(&mut ns).unwrap().clone_from(&SHARED);
                    Arc::new(RwLock::new(ns))
                });
            });
            assert!(Arc::ptr_eq(&DATA.current(), &SHARED));
            assert_eq!(Arc::strong_count(&SHARED), 2);
        })
        .join()
        .unwrap();

        assert_eq!(Arc::strong_count(&SHARED), 1);
    }

    #[test]
    fn reset() {
        static SHARED: Lazy<Arc<()>> = Lazy::new(|| Arc::new(()));
        def_resource! {
            static DATA: Arc<()> = Arc::new(());
        }

        thread::spawn(|| {
            NS.with(|ns| {
                ns.call_once(|| {
                    let mut ns = Namespace::new();
                    DATA.get_mut(&mut ns).unwrap().clone_from(&SHARED);
                    Arc::new(RwLock::new(ns))
                });
            });
            assert!(Arc::ptr_eq(&DATA.current(), &SHARED));
            assert_eq!(Arc::strong_count(&SHARED), 2);

            NS.with(|ns| {
                DATA.reset(&mut ns.get().unwrap().write());
            });
            assert_eq!(Arc::strong_count(&SHARED), 1);
        })
        .join()
        .unwrap();

        assert_eq!(Arc::strong_count(&SHARED), 1);
    }

    #[test]
    fn clone_from() {
        static SHARED: Lazy<Arc<()>> = Lazy::new(|| Arc::new(()));
        def_resource! {
            static DATA: Arc<()> = Arc::new(());
        }

        let src_ns = NS.with(|ns| {
            ns.call_once(|| {
                let mut ns = Namespace::new();
                DATA.get_mut(&mut ns).unwrap().clone_from(&SHARED);
                Arc::new(RwLock::new(ns))
            })
            .clone()
        });
        assert!(Arc::ptr_eq(&DATA.current(), &SHARED));
        assert_eq!(Arc::strong_count(&SHARED), 2);

        thread::spawn(move || {
            NS.with(|ns| {
                ns.call_once(|| {
                    let mut ns = Namespace::new();
                    DATA.share_from(&mut ns, &src_ns.read());
                    Arc::new(RwLock::new(ns))
                });
            });
            assert!(Arc::ptr_eq(&DATA.current(), &SHARED));
            assert_eq!(Arc::strong_count(&SHARED), 2);
        })
        .join()
        .unwrap();

        assert_eq!(Arc::strong_count(&SHARED), 2);
    }
}
