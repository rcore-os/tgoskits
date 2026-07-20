use ax_runtime::maintenance::{
    MAINTENANCE_BATCH_LIMIT, MAINTENANCE_MAILBOX_CAPACITY, MaintenanceCauses,
    MaintenanceDrainError, MaintenanceMailbox, MaintenancePublishResult,
};

#[test]
fn irq_publication_is_fifo_and_aggregates_causes() {
    let mailbox = MaintenanceMailbox::<u32>::new();

    assert_eq!(
        mailbox.publish_irq_event_serialized(MaintenanceCauses::IRQ, 11),
        MaintenancePublishResult::Published
    );
    assert_eq!(
        mailbox.publish_task_event(MaintenanceCauses::SUBMIT, 12),
        MaintenancePublishResult::Published
    );

    let mut events = Vec::new();
    let report = mailbox
        .drain_owner(MAINTENANCE_BATCH_LIMIT, |event| events.push(event))
        .unwrap();

    assert_eq!(events, [11, 12]);
    assert_eq!(
        report.causes(),
        MaintenanceCauses::IRQ | MaintenanceCauses::SUBMIT
    );
    assert_eq!(report.drained(), 2);
    assert!(!report.pending());
}

#[test]
fn mailbox_overflow_is_explicit_and_preserves_existing_events() {
    let mailbox = MaintenanceMailbox::<usize>::new();
    for event in 0..MAINTENANCE_MAILBOX_CAPACITY {
        assert_eq!(
            mailbox.publish_irq_event_serialized(MaintenanceCauses::IRQ, event),
            MaintenancePublishResult::Published
        );
    }

    assert_eq!(
        mailbox.publish_irq_event_serialized(MaintenanceCauses::IRQ, usize::MAX),
        MaintenancePublishResult::Overflowed
    );

    let mut events = Vec::new();
    let report = mailbox
        .drain_owner(MAINTENANCE_BATCH_LIMIT, |event| events.push(event))
        .unwrap();
    assert_eq!(
        events,
        (0..MAINTENANCE_MAILBOX_CAPACITY).collect::<Vec<_>>()
    );
    assert!(report.causes().contains(MaintenanceCauses::OVERFLOW));
    assert!(!events.contains(&usize::MAX));
}

#[test]
fn irq_and_remote_task_ingress_have_independent_capacity() {
    let mailbox = MaintenanceMailbox::<usize>::new();
    for event in 0..MAINTENANCE_MAILBOX_CAPACITY {
        assert_eq!(
            mailbox.publish_task_event(MaintenanceCauses::SUBMIT, event),
            MaintenancePublishResult::Published
        );
    }

    assert_eq!(
        mailbox.publish_irq_event_serialized(MaintenanceCauses::IRQ, MAINTENANCE_MAILBOX_CAPACITY,),
        MaintenancePublishResult::Published,
        "remote task pressure must not consume the local IRQ event reserve"
    );

    let mut events = Vec::new();
    let first = mailbox.drain_owner(1, |event| events.push(event)).unwrap();
    assert_eq!(events, [MAINTENANCE_MAILBOX_CAPACITY]);
    assert!(first.pending());
}

#[test]
fn owner_drain_rejects_unbounded_batches() {
    let mailbox = MaintenanceMailbox::<u8>::new();

    assert_eq!(
        mailbox.drain_owner(0, |_| {}),
        Err(MaintenanceDrainError::EmptyBatch)
    );
    assert_eq!(
        mailbox.drain_owner(MAINTENANCE_BATCH_LIMIT + 1, |_| {}),
        Err(MaintenanceDrainError::BatchLimitExceeded {
            requested: MAINTENANCE_BATCH_LIMIT + 1,
            maximum: MAINTENANCE_BATCH_LIMIT,
        })
    );
}
