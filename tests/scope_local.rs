use std::{
    panic,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ctor::ctor;
use scope_local::{ActiveScope, Scope, scope_local};

#[ctor]
fn init_percpu() {
    percpu::init();

    unsafe { percpu::write_percpu_reg(percpu::percpu_area_base(0)) };

    let base = percpu::read_percpu_reg();
    println!("per-CPU area base = {base:#x}");
    println!("per-CPU area size = {}", percpu::percpu_area_size());
}

#[test]
fn scope_init() {
    scope_local! {
        static DATA: usize = 42;
    }
    assert_eq!(*DATA, 42);
}

#[test]
fn scope() {
    scope_local! {
        static DATA: usize = 0;
    }

    let mut scope = Scope::new();
    assert_eq!(*DATA, 0);
    assert_eq!(*DATA.scope(&scope), 0);

    *DATA.scope_mut(&mut scope) = 42;
    assert_eq!(*DATA.scope(&scope), 42);

    unsafe { ActiveScope::set(&scope) };
    assert_eq!(*DATA, 42);

    ActiveScope::set_global();
    assert_eq!(*DATA, 0);
    assert_eq!(*DATA.scope(&scope), 42);
}

#[test]
fn scope_drop() {
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    assert_eq!(Arc::strong_count(&SHARED), 1);

    {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = SHARED.clone();

        assert_eq!(Arc::strong_count(&SHARED), 2);
        assert!(Arc::ptr_eq(&SHARED, &SHARED.scope(&scope)));
    }

    assert_eq!(Arc::strong_count(&SHARED), 1);
}

#[test]
fn scope_panic_unwind_drop() {
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    let panic = panic::catch_unwind(|| {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = SHARED.clone();
        assert_eq!(Arc::strong_count(&SHARED), 2);
        panic!("panic");
    });
    assert!(panic.is_err());

    assert_eq!(Arc::strong_count(&SHARED), 1);
}

#[test]
fn thread_share_item() {
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    let handles: Vec<_> = (0..10)
        .map(|_| {
            thread::spawn(move || {
                let global = &*SHARED;

                let mut scope = Scope::new();
                *SHARED.scope_mut(&mut scope) = global.clone();

                unsafe { ActiveScope::set(&scope) };

                assert!(Arc::strong_count(&SHARED) >= 2);
                assert!(Arc::ptr_eq(&SHARED, global));

                ActiveScope::set_global();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(Arc::strong_count(&SHARED), 1);
}

#[test]
fn thread_share_scope() {
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    let scope = Arc::new(Scope::new());

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let scope = scope.clone();
            thread::spawn(move || {
                unsafe { ActiveScope::set(&scope) };
                assert_eq!(Arc::strong_count(&SHARED), 1);
                assert!(Arc::ptr_eq(&SHARED, &SHARED.scope(&scope)));
                ActiveScope::set_global();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(Arc::strong_count(&SHARED), 1);
    assert_eq!(Arc::strong_count(&SHARED.scope(&scope)), 1);
}

#[test]
fn thread_isolation() {
    scope_local! {
        static DATA: usize = 42;
        static DATA2: AtomicUsize = AtomicUsize::new(42);
    }

    let handles: Vec<_> = (0..10)
        .map(|i| {
            thread::spawn(move || {
                let mut scope = Scope::new();
                *DATA.scope_mut(&mut scope) = i;

                unsafe { ActiveScope::set(&scope) };
                assert_eq!(*DATA, i);

                DATA2.store(i, Ordering::Relaxed);

                ActiveScope::set_global();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(*DATA, 42);
    assert_eq!(DATA2.load(Ordering::Relaxed), 42);
}
