use super::*;

#[test]
fn independent_readers_share_ownership_until_the_last_release() {
    let gate = Arc::new(AccessGate::new());
    let first = gate.read().unwrap();
    let second = gate.read().unwrap();
    assert_eq!(gate.state.lock().readers, 2);
    drop(first);
    assert_eq!(gate.state.lock().readers, 1);
    drop(second);
    let writer = gate.write().unwrap();
    assert!(gate.state.lock().writer);
    drop(writer);
    assert_eq!(gate.state.lock().readers, 0);
    assert!(!gate.state.lock().writer);
}

#[test]
fn queued_writer_excludes_new_readers_and_waits_without_state_exclusion() {
    let gate = Arc::new(AccessGate::new());
    let mut reader = Some(gate.read().unwrap());
    let writer = gate
        .write_with(|| {
            assert!(
                gate.state.try_lock().is_some(),
                "wait retained IRQ exclusion"
            );
            assert_eq!(gate.state.lock().waiting_writers, 1);
            assert_eq!(
                gate.read_with(|| Err(VfsError::WouldBlock)).err(),
                Some(VfsError::WouldBlock)
            );
            drop(reader.take().expect("writer waited more than once"));
            Ok(())
        })
        .unwrap();
    assert!(reader.is_none());
    assert_eq!(gate.state.lock().waiting_writers, 0);
    drop(writer);
    drop(gate.read().unwrap());
}

#[test]
fn failed_writer_wait_reopens_reader_admission_without_losing_the_error() {
    let gate = Arc::new(AccessGate::new());
    let reader = gate.read().unwrap();
    let result = gate.write_with(|| Err(VfsError::NoMemory));
    assert_eq!(result.err(), Some(VfsError::NoMemory));
    assert_eq!(gate.state.lock().waiting_writers, 0);
    assert!(!gate.state.lock().writer);
    let second = gate.read().unwrap();
    assert_eq!(gate.state.lock().readers, 2);
    drop(second);
    drop(reader);
    drop(gate.write().unwrap());
}

#[test]
fn failed_reader_wait_does_not_claim_a_reader_slot() {
    let gate = Arc::new(AccessGate::new());
    let writer = gate.write().unwrap();
    assert_eq!(
        gate.read_with(|| Err(VfsError::NoMemory)).err(),
        Some(VfsError::NoMemory)
    );
    assert_eq!(gate.state.lock().readers, 0);
    drop(writer);
    drop(gate.read().unwrap());
}

#[test]
fn read_and_writer_count_overflow_preserve_admission_state() {
    let gate = Arc::new(AccessGate::new());
    gate.state.lock().readers = usize::MAX;
    assert_eq!(gate.try_read().err(), Some(VfsError::ValueOverflow));
    assert_eq!(gate.state.lock().readers, usize::MAX);
    gate.state.lock().readers = 0;
    gate.state.lock().waiting_writers = usize::MAX;
    assert_eq!(
        gate.write_with(|| panic!("overflow must not wait")).err(),
        Some(VfsError::ValueOverflow)
    );
    assert_eq!(gate.state.lock().waiting_writers, usize::MAX);
    assert!(gate.try_read().unwrap().is_none());
    assert!(gate.try_write().is_none());
    gate.state.lock().waiting_writers = 0;
    drop(gate.write().unwrap());
}

#[test]
fn writer_waits_for_every_reader_and_spurious_wakes_do_not_admit_it() {
    let gate = Arc::new(AccessGate::new());
    let mut first = Some(gate.read().unwrap());
    let mut second = Some(gate.read().unwrap());
    let mut waits = 0;
    let writer = gate
        .write_with(|| {
            waits += 1;
            assert!(gate.state.try_lock().is_some());
            assert!(gate.try_read().unwrap().is_none());
            assert!(gate.try_write().is_none());
            match waits {
                1 => drop(first.take()),
                2 => assert_eq!(gate.state.lock().readers, 1),
                3 => drop(second.take()),
                _ => panic!("writer did not observe the final reader release"),
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(waits, 3);
    assert!(gate.try_read().unwrap().is_none());
    drop(writer);
    drop(gate.read().unwrap());
}

#[test]
fn real_waiter_wakes_after_the_final_reader_releases() {
    use std::{
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    crate::os::task::install_test_runtime_ops();
    let gate = Arc::new(AccessGate::new());
    let first = gate.read().unwrap();
    let second = gate.read().unwrap();
    let (acquired, result) = mpsc::channel();
    let writer_gate = gate.clone();
    let writer = thread::spawn(move || {
        let _writer = writer_gate.write().unwrap();
        acquired.send(()).unwrap();
    });
    // Scheduling only chooses when registration happens, not the tested order.
    // The deadline is a failure watchdog, never the progress predicate.
    let deadline = Instant::now() + Duration::from_secs(5);
    while gate.changed.len() == 0 {
        assert!(
            Instant::now() < deadline,
            "writer never registered its wait"
        );
        thread::yield_now();
    }
    assert_eq!(gate.state.lock().waiting_writers, 1);
    assert!(gate.try_read().unwrap().is_none());
    drop(first);
    assert_eq!(gate.state.lock().readers, 1);
    assert!(!gate.state.lock().writer);
    assert!(result.try_recv().is_err());
    drop(second);
    result.recv_timeout(Duration::from_secs(5)).unwrap();
    writer.join().unwrap();
    assert_eq!(gate.changed.len(), 0);
    assert_eq!(gate.state.lock().waiting_writers, 0);
    drop(gate.read().unwrap());
}
