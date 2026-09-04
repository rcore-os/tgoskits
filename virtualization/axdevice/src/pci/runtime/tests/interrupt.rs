use super::*;

#[test]
fn irq_transition_permit_surfaces_real_line_backend_failures() {
    let sink = Arc::new(FailingDeassertSink {
        fail_deassert: AtomicBool::new(false),
        asserted: AtomicBool::new(false),
    });
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(19),
        InterruptTrigger::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let mut permit = EndpointIrqTransitionPermit { _private: () };

    permit.assert(&line).unwrap();
    sink.fail_deassert.store(true, Ordering::Relaxed);
    let error = permit.deassert(&line).unwrap_err();
    assert!(matches!(
        error,
        DeviceError::Backend {
            operation: "deassert PCI endpoint INTx line",
            ..
        }
    ));
}

#[test]
fn orphan_retry_merges_concurrent_transfers_without_dropping_owners() {
    let _test_lock = ORPHAN_QUEUE_TEST_LOCK.lock().unwrap();
    PciRootBinding::retry_orphaned_irq_withdrawals().unwrap();

    let first = Arc::new(BlockingWithdrawalFunction {
        started: AtomicBool::new(false),
        release: AtomicBool::new(false),
        withdrawals: AtomicUsize::new(0),
    });
    ORPHANED_IRQ_WITHDRAWALS
        .lock_irqsave()
        .push(pending_withdrawal(1, first.clone()));

    let retry = std::thread::spawn(PciRootBinding::retry_orphaned_irq_withdrawals);
    while !first.started.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    let second = Arc::new(BlockingWithdrawalFunction {
        started: AtomicBool::new(false),
        release: AtomicBool::new(true),
        withdrawals: AtomicUsize::new(0),
    });
    let incoming = SpinLock::new(vec![pending_withdrawal(2, second.clone())]);
    transfer_pending_irq_withdrawals(&incoming);
    first.release.store(true, Ordering::Release);
    retry.join().unwrap().unwrap();

    assert_eq!(first.withdrawals.load(Ordering::Relaxed), 1);
    assert_eq!(second.withdrawals.load(Ordering::Relaxed), 0);
    PciRootBinding::retry_orphaned_irq_withdrawals().unwrap();
    assert_eq!(second.withdrawals.load(Ordering::Relaxed), 1);
}
