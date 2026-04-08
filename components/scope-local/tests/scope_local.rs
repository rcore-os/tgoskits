use std::{
    panic,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ctor::ctor;
use scope_local::{ActiveScope, Scope, scope_local};

static TEST_LOCK: Mutex<()> = Mutex::new(());

#[ctor]
fn init_percpu() {
    ax_percpu::init();
    ax_percpu::init_percpu_reg(0);

    let base = ax_percpu::read_percpu_reg();
    println!("per-CPU area base = {base:#x}");
    println!("per-CPU area size = {}", ax_percpu::percpu_area_size());
}

#[test]
fn scope_init() {
    let _guard = TEST_LOCK.lock().unwrap();
    scope_local! {
        static DATA: usize = 42;
    }
    assert_eq!(*DATA, 42);
}

#[test]
fn scope() {
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
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
    let _guard = TEST_LOCK.lock().unwrap();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }
    let cpu_num = ax_percpu::percpu_area_num().max(1);

    let handles: Vec<_> = (0..cpu_num)
        .map(|cpu_id| {
            thread::spawn(move || {
                ax_percpu::init_percpu_reg(cpu_id);
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
    let _guard = TEST_LOCK.lock().unwrap();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }
    let cpu_num = ax_percpu::percpu_area_num().max(1);

    let scope = Arc::new(Scope::new());

    let handles: Vec<_> = (0..cpu_num)
        .map(|cpu_id| {
            let scope = scope.clone();
            thread::spawn(move || {
                ax_percpu::init_percpu_reg(cpu_id);
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
    let _guard = TEST_LOCK.lock().unwrap();
    scope_local! {
        static DATA: usize = 42;
        static DATA2: AtomicUsize = AtomicUsize::new(42);
    }
    let cpu_num = ax_percpu::percpu_area_num().max(1);

    let handles: Vec<_> = (0..cpu_num)
        .map(|i| {
            thread::spawn(move || {
                ax_percpu::init_percpu_reg(i);
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
