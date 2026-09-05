use super::*;

fn barrier_test_inner() -> Arc<DeviceInner> {
    let ops = runtime_ops().unwrap();
    let controller_notification = ops.notification();
    Arc::new(DeviceInner {
        name: String::from("barrier-test"),
        device_info: IrqMutex::new(DeviceInfoEpoch::new(test_queue_info().device)),
        max_io_queues: 1,
        irq_sources: Vec::new(),
        hctxs: IrqMutex::new(Vec::new()),
        detached_queues: IrqMutex::new(Vec::new()),
        cpu_channels: IrqMutex::new(Vec::new()),
        irq_registrations: IrqMutex::new(Vec::new()),
        controller: Arc::new(ControllerPort {
            commands: BoundedChannel::with_item_notification(
                1,
                Arc::clone(&controller_notification),
            )
            .unwrap(),
            notification: controller_notification,
            irq_latches: IrqMutex::new(Vec::new()),
            terminal_confirmed: AtomicBool::new(false),
        }),
        controller_thread: IrqMutex::new(None),
        state: AtomicU8::new(DEVICE_READY),
        accepting: AtomicBool::new(true),
        data_gate_waiters: TaskWaiters::new(),
        flush_gate_waiters: TaskWaiters::new(),
        data_drain_waiters: TaskWaiters::new(),
        state_notification: ops.notification(),
        lifecycle_gate: IrqMutex::new(LifecycleGateState {
            phase: DevicePhase::Ready,
            submission_ready_hctx_count: 0,
            active_data: 0,
            flush_active: false,
            teardown_in_progress: false,
            terminal_teardown_error: None,
        }),
        shutdown_waiters: TaskWaiters::new(),
        member_id: None,
        group_owner: None,
    })
}

#[test]
fn flush_barrier_waits_for_prior_data_and_holds_later_data() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner
        .enter_data_submissions(1, SubmissionAdmission::Blocking)
        .unwrap();

    let flush_inner = Arc::clone(&inner);
    let (flush_tx, flush_rx) = mpsc::channel();
    let flush_thread = thread::spawn(move || {
        flush_inner
            .begin_flush_barrier(SubmissionAdmission::Blocking)
            .unwrap();
        flush_tx.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !inner.lifecycle_gate.lock().flush_active {
        assert!(Instant::now() < deadline, "flush gate was not acquired");
        thread::yield_now();
    }

    let later_inner = Arc::clone(&inner);
    let (later_tx, later_rx) = mpsc::channel();
    let later_thread = thread::spawn(move || {
        later_inner
            .enter_data_submissions(1, SubmissionAdmission::Blocking)
            .unwrap();
        later_tx.send(()).unwrap();
    });
    assert!(flush_rx.recv_timeout(Duration::from_millis(20)).is_err());
    assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

    inner.request_completed(RequestOp::Write, 1, Ok(()));
    flush_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

    inner.request_completed(RequestOp::Flush, 0, Ok(()));
    later_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    inner.request_completed(RequestOp::Read, 1, Ok(()));
    flush_thread.join().unwrap();
    later_thread.join().unwrap();
}

#[test]
fn nowait_admission_never_sleeps_behind_flush_barrier() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    assert_eq!(
        inner.enter_data_submissions(1, SubmissionAdmission::Nowait),
        Err(BlkError::Retry)
    );
    assert_eq!(inner.lifecycle_gate.lock().active_data, 0);

    inner.lifecycle_gate.lock().flush_active = false;
    inner.lifecycle_gate.lock().active_data = 1;
    assert_eq!(
        inner.begin_flush_barrier(SubmissionAdmission::Nowait),
        Err(BlkError::Retry)
    );
    assert!(!inner.lifecycle_gate.lock().flush_active);
}

#[test]
fn flush_completion_wakes_every_blocked_data_submitter() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    let (done_tx, done_rx) = mpsc::channel();
    let mut joins = Vec::new();
    for _ in 0..3 {
        let waiter = Arc::clone(&inner);
        let done_tx = done_tx.clone();
        joins.push(thread::spawn(move || {
            waiter
                .enter_data_submissions(1, SubmissionAdmission::Blocking)
                .unwrap();
            done_tx.send(()).unwrap();
        }));
    }
    drop(done_tx);
    let deadline = Instant::now() + Duration::from_secs(1);
    while inner.data_gate_waiters.len() != 3 {
        assert!(
            Instant::now() < deadline,
            "data submitters did not enter the barrier wait set"
        );
        thread::yield_now();
    }

    inner.request_completed(RequestOp::Flush, 0, Ok(()));
    for _ in 0..3 {
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    for _ in 0..3 {
        inner.request_completed(RequestOp::Read, 1, Ok(()));
    }
    for join in joins {
        join.join().unwrap();
    }
}
