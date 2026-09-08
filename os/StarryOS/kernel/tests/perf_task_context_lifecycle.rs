#[path = "../src/perf/task_context_state.rs"]
mod task_context_state;

use task_context_state::{PerfAttachError, PerfTaskContextState};

#[test]
fn close_rejects_future_attach_and_keeps_the_winning_snapshot() {
    let mut context = PerfTaskContextState::<u32, 4>::new();
    context.attach(1).unwrap();

    let exiting = context.close_snapshot();

    assert_eq!(exiting.as_slice(), &[1]);
    assert_eq!(context.attach(2), Err(PerfAttachError::Closed));
}

#[test]
fn fixed_context_reclaims_retired_entries_without_reopening_admission() {
    let mut context = PerfTaskContextState::<u32, 2>::new();
    context.attach(1).unwrap();
    context.attach(2).unwrap();
    assert_eq!(context.attach(3), Err(PerfAttachError::Full));
    assert_eq!(context.snapshot_if_accepting().unwrap().as_slice(), &[1, 2]);

    context.retain(|counter| *counter != 1);
    assert_eq!(context.counters(), &[2]);
    assert!(context.remove(|counter| *counter == 2));
    assert!(!context.remove(|counter| *counter == 2));
    assert!(context.snapshot().is_empty());

    assert!(context.close_snapshot().is_empty());
    assert_eq!(context.attach(4), Err(PerfAttachError::Closed));
}

#[test]
fn attach_racing_close_has_exactly_one_owner() {
    loom::model(|| {
        use loom::{
            sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            },
            thread,
        };

        let context = Arc::new(Mutex::new(PerfTaskContextState::<u32, 4>::new()));
        let active = Arc::new(AtomicUsize::new(0));
        let attach_context = Arc::clone(&context);
        let attach_active = Arc::clone(&active);
        let attach = thread::spawn(move || {
            let mut context = attach_context.lock().unwrap();
            let result = context.attach(7);
            if result.is_ok() {
                // This mirrors ThreadPerfContext: the fast-path key is
                // published before releasing the same lock as the list entry.
                attach_active.fetch_add(1, Ordering::AcqRel);
            }
            result
        });
        let close_context = Arc::clone(&context);
        let close_active = Arc::clone(&active);
        let close = thread::spawn(move || {
            let exiting = close_context.lock().unwrap().close_snapshot();
            for _ in &exiting {
                assert_eq!(close_active.fetch_sub(1, Ordering::AcqRel), 1);
            }
            exiting
        });

        let attach_result = attach.join().unwrap();
        let exiting = close.join().unwrap();
        match attach_result {
            Ok(()) => assert_eq!(exiting.as_slice(), &[7]),
            Err(PerfAttachError::Closed) => assert!(exiting.is_empty()),
            Err(PerfAttachError::Full) => panic!("one counter cannot fill the context"),
        }
        assert_eq!(
            context.lock().unwrap().attach(8),
            Err(PerfAttachError::Closed)
        );
        assert_eq!(active.load(Ordering::Acquire), 0);
    });
}
