extern crate alloc;

use alloc::sync::Arc;
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
};

#[path = "../src/perf/output.rs"]
mod output;

use output::{PerfOutputRoute, PerfOutputScope, PerfRingOutput, validate_output_redirect};

struct DropProbe(Arc<AtomicBool>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn output_snapshot_pins_ring_until_the_writer_finishes() {
    let dropped = Arc::new(AtomicBool::new(false));
    let owner = Arc::new(DropProbe(Arc::clone(&dropped)));
    let anchor: Arc<dyn Any + Send + Sync> = owner.clone();
    let output = PerfRingOutput::new(0x1000, 0x2000, anchor);

    drop(owner);
    assert!(
        !dropped.load(Ordering::Acquire),
        "an output snapshot must retain the ring after fd/task teardown"
    );
    assert_eq!(output.ring_vaddr(), 0x1000);
    assert_eq!(output.ring_len(), 0x2000);

    drop(output);
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn cloned_outputs_share_one_bounded_writer_gate() {
    let anchor: Arc<dyn Any + Send + Sync> = Arc::new(());
    let output = PerfRingOutput::new(0x3000, 0x4000, anchor);
    let clone = output.clone();

    let writer = output
        .try_begin_write()
        .expect("the first producer must acquire the output");
    assert!(
        clone.try_begin_write().is_none(),
        "an IRQ producer must drop instead of racing or waiting for another CPU"
    );
    clone.record_contention_drop();
    assert_eq!(clone.contention_drops(), 1);
    drop(writer);
    assert!(
        clone.try_begin_write().is_some(),
        "releasing one snapshot must publish the shared ring to every producer"
    );
}

#[test]
fn detach_restores_the_events_own_ring() {
    let own = PerfRingOutput::new(0x5000, 0x2000, Arc::new(()));
    let redirected = PerfRingOutput::new(0x9000, 0x2000, Arc::new(()));
    let mut route = PerfOutputRoute::new();
    route.publish_owned(&own);
    route.redirect(redirected.clone());

    let (selected, is_redirected) = route.effective().unwrap();
    assert!(is_redirected);
    assert_eq!(selected.ring_vaddr(), redirected.ring_vaddr());

    route.detach();
    let (selected, is_redirected) = route.effective().unwrap();
    assert!(!is_redirected);
    assert_eq!(selected.ring_vaddr(), own.ring_vaddr());
}

#[test]
fn output_redirect_requires_distinct_events_in_one_owner_context() {
    let task = PerfOutputScope::Task(0x1_0000_0007);
    let other_task = PerfOutputScope::Task(0x2_0000_0007);
    let cpu = PerfOutputScope::Cpu(1);

    assert!(validate_output_redirect(10, 11, task, task).is_ok());
    assert!(validate_output_redirect(10, 10, task, task).is_err());
    assert!(validate_output_redirect(10, 11, task, other_task).is_err());
    assert!(validate_output_redirect(10, 11, task, cpu).is_err());
}
