use core::num::NonZeroU32;
use std::sync::{
    Mutex, MutexGuard,
    atomic::{AtomicUsize, Ordering},
};

use ctor::ctor;
use scope_local::{ActiveScope, Scope, scope_local};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static PREEMPT_DEPTH: AtomicUsize = AtomicUsize::new(0);
static EAGER_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);
static PINNED_INIT_COUNT: AtomicUsize = AtomicUsize::new(0);

struct KernelGuardIfImpl;

#[ax_crate_interface::impl_interface]
impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
    fn enable_preempt() {
        PREEMPT_DEPTH.fetch_sub(1, Ordering::AcqRel);
    }

    fn disable_preempt() {
        PREEMPT_DEPTH.fetch_add(1, Ordering::AcqRel);
    }
}

scope_local! {
    static INIT_PREEMPT_DEPTH: usize = PREEMPT_DEPTH.load(Ordering::Acquire);
    static EAGER_VALUE: usize = {
        EAGER_INIT_COUNT.fetch_add(1, Ordering::AcqRel);
        7
    };
    static PINNED_VALUE: usize = {
        PINNED_INIT_COUNT.fetch_add(1, Ordering::AcqRel);
        11
    };
}

#[ctor]
fn init_percpu() {
    let area_count = NonZeroU32::new(1).unwrap();
    ax_percpu::host_test::initialize(area_count).unwrap();
    let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0).unwrap()).unwrap();
    // SAFETY: this test binary models one CPU and never replaces its area.
    unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
}

fn test_guard() -> MutexGuard<'static, ()> {
    let guard = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let area = ax_percpu::area(ax_percpu::CpuIndex::try_from(0).unwrap()).unwrap();
    match unsafe { cpu_local::with_cpu_pin(|pin| pin.area()) } {
        Ok(installed) => assert_eq!(installed, area.cpu_area().unwrap()),
        Err(cpu_local::CpuLocalError::AreaNotInstalled) => {
            // SAFETY: every serialized test thread binds the same modeled CPU.
            unsafe { cpu_local::install_cpu_area(area.cpu_area().unwrap()) }.unwrap();
        }
        Err(error) => panic!("invalid host CPU-local binding: {error}"),
    }
    guard
}

#[test]
fn initializer_runs_outside_preemption_guard() {
    let _guard = test_guard();

    assert_eq!(INIT_PREEMPT_DEPTH.with(|depth| *depth), 0);
}

#[test]
fn scope_new_initializes_every_item() {
    let _guard = test_guard();
    EAGER_INIT_COUNT.store(0, Ordering::Release);

    let scope = Scope::new();

    assert_eq!(EAGER_INIT_COUNT.load(Ordering::Acquire), 1);
    assert_eq!(*EAGER_VALUE.scope(&scope), 7);
    assert_eq!(EAGER_INIT_COUNT.load(Ordering::Acquire), 1);
}

#[test]
fn pinned_access_has_no_initialization_side_effects() {
    let _guard = test_guard();
    PINNED_INIT_COUNT.store(0, Ordering::Release);
    let scope = Scope::new();
    assert_eq!(PINNED_INIT_COUNT.load(Ordering::Acquire), 1);

    // SAFETY: `scope` remains live until the global scope is restored.
    unsafe { ActiveScope::set(&scope) };
    assert_eq!(PINNED_VALUE.with(|value| *value), 11);
    // SAFETY: this serialized host test cannot migrate between modeled CPUs.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            assert_eq!(PINNED_VALUE.with_pinned(pin, |value| *value), 11);
            assert_eq!(PINNED_VALUE.try_with_pinned(pin, |value| *value), Some(11));
        })
    }
    .unwrap();
    ActiveScope::set_global();

    assert_eq!(PINNED_INIT_COUNT.load(Ordering::Acquire), 1);
}
