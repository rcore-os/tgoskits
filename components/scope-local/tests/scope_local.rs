use core::num::NonZeroU32;
use std::{
    panic,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use ctor::ctor;
use scope_local::{ActiveScope, Scope, scope_local};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static UNUSED_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);
static NEXT_TEST_CPU: AtomicUsize = AtomicUsize::new(1);

#[ctor]
fn init_percpu() {
    let area_count = NonZeroU32::new(4).unwrap();
    let layout = ax_percpu::host_test::initialize(area_count).unwrap();
    CPU_COUNT.store(layout.area_count() as usize, Ordering::Release);

    let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0).unwrap()).unwrap();
    println!("per-CPU area base = {:#x}", area.runtime_base());
    println!("per-CPU area size = {}", area.area_size());
}

fn bind_test_cpu(cpu_id: usize) {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id).unwrap();
    let area = ax_percpu::area(cpu_index).unwrap();
    match unsafe { cpu_local::with_cpu_pin(|pin| pin.area()) } {
        Ok(installed) => assert_eq!(installed, area.cpu_area().unwrap()),
        Err(cpu_local::CpuLocalError::AreaNotInstalled) => {
            // SAFETY: each test thread binds one initialized area and never
            // changes that physical-CPU model during the thread's lifetime.
            unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
        }
        Err(error) => panic!("invalid host CPU-local binding: {error}"),
    }
}

fn fresh_test_cpu() -> usize {
    let cpu_id = NEXT_TEST_CPU.fetch_add(1, Ordering::AcqRel);
    assert!(
        cpu_id < CPU_COUNT.load(Ordering::Acquire),
        "scope-local host tests exhausted one-shot CPU areas"
    );
    cpu_id
}

fn test_guard() -> MutexGuard<'static, ()> {
    let guard = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    bind_test_cpu(0);
    guard
}

#[test]
fn scope_init() {
    let _guard = test_guard();
    scope_local! {
        static DATA: usize = 42;
    }
    assert_eq!(DATA.with(|value| *value), 42);
}

#[test]
fn scope_init_is_per_item_lazy() {
    let _guard = test_guard();
    UNUSED_INIT_COUNT.store(0, Ordering::Relaxed);
    scope_local! {
        static DATA: usize = 42;
        static UNUSED: usize = {
            UNUSED_INIT_COUNT.fetch_add(1, Ordering::Relaxed);
            7
        };
    }

    assert_eq!(DATA.with(|value| *value), 42);
    assert_eq!(UNUSED_INIT_COUNT.load(Ordering::Relaxed), 0);
}

#[test]
fn pinned_access_does_not_initialize_an_item() {
    let _guard = test_guard();
    scope_local! {
        static DATA: usize = 7;
    }

    // SAFETY: the serialized host test cannot migrate between modeled CPUs.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            assert_eq!(DATA.try_with_pinned(pin, |value| *value), None);
            assert_eq!(DATA.with(|value| *value), 7);
            assert_eq!(DATA.try_with_pinned(pin, |value| *value), Some(7));
        })
    }
    .unwrap();
}

#[test]
fn scope() {
    let _guard = test_guard();
    scope_local! {
        static DATA: usize = 0;
    }

    let mut scope = Scope::new();
    assert_eq!(DATA.with(|value| *value), 0);
    assert_eq!(*DATA.scope(&scope), 0);

    *DATA.scope_mut(&mut scope) = 42;
    assert_eq!(*DATA.scope(&scope), 42);

    // SAFETY: `scope` remains live until the global scope is restored.
    unsafe { ActiveScope::set(&scope) };
    assert_eq!(DATA.with(|value| *value), 42);

    ActiveScope::set_global();
    assert_eq!(DATA.with(|value| *value), 0);
    assert_eq!(*DATA.scope(&scope), 42);
}

#[test]
fn scope_drop() {
    let _guard = test_guard();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    assert_eq!(SHARED.with(Arc::strong_count), 1);

    {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = SHARED.clone_current();

        assert_eq!(SHARED.with(Arc::strong_count), 2);
        assert!(SHARED.with(|shared| Arc::ptr_eq(shared, &SHARED.scope(&scope))));
    }

    assert_eq!(SHARED.with(Arc::strong_count), 1);
}

#[test]
fn scope_panic_unwind_drop() {
    let _guard = test_guard();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }

    let panic = panic::catch_unwind(|| {
        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = SHARED.clone_current();
        assert_eq!(SHARED.with(Arc::strong_count), 2);
        panic!("panic");
    });
    assert!(panic.is_err());

    assert_eq!(SHARED.with(Arc::strong_count), 1);
}

#[test]
fn thread_share_item() {
    let _guard = test_guard();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }
    let cpu_id = fresh_test_cpu();
    thread::spawn(move || {
        bind_test_cpu(cpu_id);
        let global = SHARED.clone_current();

        let mut scope = Scope::new();
        *SHARED.scope_mut(&mut scope) = global.clone();

        // SAFETY: `scope` remains live until the global scope is restored.
        unsafe { ActiveScope::set(&scope) };
        assert!(SHARED.with(Arc::strong_count) >= 2);
        assert!(SHARED.with(|shared| Arc::ptr_eq(shared, &global)));
        ActiveScope::set_global();
    })
    .join()
    .unwrap();

    assert_eq!(SHARED.with(Arc::strong_count), 1);
}

#[test]
fn thread_share_scope() {
    let _guard = test_guard();
    scope_local! {
        static SHARED: Arc<()> = Arc::new(());
    }
    let scope = Arc::new(Scope::new());
    let cpu_id = fresh_test_cpu();
    let worker_scope = Arc::clone(&scope);
    thread::spawn(move || {
        bind_test_cpu(cpu_id);
        // SAFETY: `worker_scope` remains live until the global scope is restored.
        unsafe { ActiveScope::set(&worker_scope) };
        assert_eq!(SHARED.with(Arc::strong_count), 1);
        assert!(SHARED.with(|shared| Arc::ptr_eq(shared, &SHARED.scope(&worker_scope))));
        ActiveScope::set_global();
    })
    .join()
    .unwrap();

    assert_eq!(SHARED.with(Arc::strong_count), 1);
    assert_eq!(Arc::strong_count(&SHARED.scope(&scope)), 1);
}

#[test]
fn thread_isolation() {
    let _guard = test_guard();
    scope_local! {
        static DATA: usize = 42;
        static DATA2: AtomicUsize = AtomicUsize::new(42);
    }
    let cpu_id = fresh_test_cpu();
    thread::spawn(move || {
        bind_test_cpu(cpu_id);
        let mut scope = Scope::new();
        *DATA.scope_mut(&mut scope) = cpu_id;

        // SAFETY: `scope` remains live until the global scope is restored.
        unsafe { ActiveScope::set(&scope) };
        assert_eq!(DATA.with(|value| *value), cpu_id);
        DATA2.with(|value| value.store(cpu_id, Ordering::Relaxed));
        ActiveScope::set_global();
    })
    .join()
    .unwrap();

    assert_eq!(DATA.with(|value| *value), 42);
    assert_eq!(DATA2.with(|value| value.load(Ordering::Relaxed)), 42);
}
