//! Thread publication and physical/OS resource lifetime integration tests.
use std::{
    boxed::Box,
    println,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use ax_std::os::arceos::task::{
    sched::SchedulePolicy,
    thread::{SwitchReason, ThreadExtension, ThreadExtensionOps, ThreadId},
};

#[path = "lifecycle_review.rs"]
mod review;

pub(super) fn run() {
    review::run();
    test_execution_reclamation_with_live_handle();
    test_cancel_unpublished_threads();
    test_common_exit_result();
    test_creation_failure_rollback();
    test_direct_extension_lifetime(false);
    test_direct_extension_lifetime(true);
}

fn test_execution_reclamation_with_live_handle() {
    let handle = ax_std::os::arceos::thread::builder("reclaim-live-handle".into())
        .spawn(|| {})
        .unwrap();
    assert_eq!(handle.wait().unwrap(), 0);
    let started = std::time::Instant::now();
    while !handle.execution_reclaimed() && started.elapsed() < Duration::from_secs(2) {
        thread::yield_now();
    }
    assert!(
        handle.execution_reclaimed(),
        "a management handle must not retain an exited kernel stack"
    );
    assert_eq!(handle.join().unwrap(), 0);
    println!("task_wait_queue: execution resources reclaimed before task handle OK");
}

fn test_cancel_unpublished_threads() {
    struct EntryCapture(Arc<AtomicUsize>);
    impl Drop for EntryCapture {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }
    for stage in [false, true] {
        let dropped = Arc::new(AtomicUsize::new(0));
        let capture = EntryCapture(Arc::clone(&dropped));
        let prepared = ax_std::os::arceos::thread::builder("cancel-unpublished".into())
            .prepare(move || {
                drop(capture);
                panic!("cancelled entry must never run");
            })
            .unwrap();
        let handle = prepared.thread_handle();
        if stage {
            drop(prepared.stage().unwrap());
        } else {
            drop(prepared);
        }
        assert_eq!(handle.wait().unwrap(), 0);
        assert_eq!(
            dropped.load(Ordering::Acquire),
            1,
            "cancel must destroy the captured entry exactly once"
        );
        handle.join().unwrap();
    }
    println!("task_wait_queue: unpublished cancellation OK");
}

fn test_common_exit_result() {
    let handle = ax_std::os::arceos::thread::builder("exit-result".into())
        .spawn(|| {
            let current =
                ax_std::os::arceos::task::thread::current::current_thread_handle().unwrap();
            assert!(matches!(
                current.wait(),
                Err(ax_std::os::arceos::task::thread::TaskError::InvalidConfiguration)
            ));
            drop(current);
            ax_std::os::arceos::task::thread::current::exit_current(17)
        })
        .unwrap();
    assert_eq!(handle.wait().unwrap(), 17);
    assert_eq!(handle.join().unwrap(), 17);
    println!("task_wait_queue: common exit result and self-join rejection OK");
}

fn test_creation_failure_rollback() {
    for fail_before_allocation in [true, false] {
        let counters = Arc::new(ExtensionProbe::default());
        let data = Box::into_raw(Box::new(Arc::clone(&counters))) as usize;
        // SAFETY: the extension uniquely owns this boxed Arc through PROBE_OPS.
        let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
        let builder =
            ax_std::os::arceos::thread::builder("failed-creation".into()).extension(extension);
        let builder = if fail_before_allocation {
            builder.stack_size(0)
        } else {
            // A mismatched topology fails after the real context/TLS/stack constructor.
            builder.affinity(ax_std::os::arceos::task::sched::CpuSet::empty(
                ax_hal::cpu_num() + 1,
            ))
        };
        assert!(
            builder
                .prepare(|| panic!("failed creation must not run"))
                .is_err()
        );
        assert_eq!(counters.switched_in.load(Ordering::Acquire), 0);
        assert_eq!(counters.exited.load(Ordering::Acquire), 0);
        assert_eq!(counters.dropped.load(Ordering::Acquire), 1);
    }
    println!("task_wait_queue: creation failure releases unpublished extension once OK");
}

#[derive(Default)]
struct ExtensionProbe {
    pause_exit: std::sync::atomic::AtomicBool,
    switched_in: AtomicUsize,
    switched_out: AtomicUsize,
    exited: AtomicUsize,
    dropped: AtomicUsize,
}
static PROBE_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: probe_in,
    on_switch_out: probe_out,
    on_exit: probe_exit,
    on_deadline_overrun: probe_deadline,
    drop: probe_drop,
};
unsafe fn probe<'a>(data: usize) -> &'a Arc<ExtensionProbe> {
    // SAFETY: every callback receives the boxed Arc retained by PROBE_OPS.
    unsafe { &*(data as *const Arc<ExtensionProbe>) }
}
unsafe extern "Rust" fn probe_in(data: usize, _: ThreadId, _: SchedulePolicy, _: u64) {
    assert!(!ax_hal::cpu::interrupt::irqs_enabled());
    unsafe { probe(data) }
        .switched_in
        .fetch_add(1, Ordering::Release);
}
unsafe extern "Rust" fn probe_out(data: usize, _: ThreadId, _: SwitchReason) {
    assert!(!ax_hal::cpu::interrupt::irqs_enabled());
    unsafe { probe(data) }
        .switched_out
        .fetch_add(1, Ordering::Release);
}
unsafe extern "Rust" fn probe_exit(data: usize, _: ThreadId) {
    assert!(ax_hal::cpu::interrupt::irqs_enabled() && !ax_hal::irq::in_irq_context());
    unsafe { probe(data) }
        .exited
        .fetch_add(1, Ordering::Release);
    while unsafe { probe(data) }.pause_exit.load(Ordering::Acquire) {
        thread::yield_now();
    }
}
unsafe extern "Rust" fn probe_deadline(_: usize, _: ThreadId) {}
unsafe extern "Rust" fn probe_drop(data: usize) {
    assert!(ax_hal::cpu::interrupt::irqs_enabled() && !ax_hal::irq::in_irq_context());
    // SAFETY: the unique extension destructor consumes the boxed Arc exactly once.
    let probe = unsafe { Box::from_raw(data as *mut Arc<ExtensionProbe>) };
    probe.dropped.fetch_add(1, Ordering::Release);
}
fn wait_for(condition: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !condition() && started.elapsed() < Duration::from_secs(2) {
        thread::yield_now();
    }
    assert!(condition(), "thread lifetime event must complete");
}
fn test_direct_extension_lifetime(drop_in_sensitive_context: bool) {
    let counters = Arc::new(ExtensionProbe::default());
    let data = Box::into_raw(Box::new(Arc::clone(&counters))) as usize;
    // SAFETY: the callbacks use this boxed Arc until their unique drop callback.
    let extension = unsafe { ThreadExtension::new(data, &PROBE_OPS) };
    let handle = ax_std::os::arceos::thread::builder("direct-extension".into())
        .extension(extension)
        .spawn(thread::yield_now)
        .unwrap();
    assert_eq!(handle.wait().unwrap(), 0);
    wait_for(|| handle.execution_reclaimed());
    assert!(counters.switched_in.load(Ordering::Acquire) > 0);
    assert!(counters.switched_out.load(Ordering::Acquire) > 0);
    assert_eq!(counters.exited.load(Ordering::Acquire), 1);
    assert_eq!(counters.dropped.load(Ordering::Acquire), 0);
    let borrowed = handle
        .extension()
        .expect("direct OS extension remains available");
    assert_eq!(borrowed.data(), data);
    if drop_in_sensitive_context {
        let _guard = ax_std::os::arceos::guard::PreemptIrqSaveGuard::new();
        drop(handle);
    } else {
        handle.join().unwrap();
    }
    wait_for(|| counters.dropped.load(Ordering::Acquire) == 1);
    println!("task_wait_queue: direct extension switch/exit/drop lifetime OK");
}
