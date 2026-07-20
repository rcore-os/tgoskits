use rdif_eth::{Event, IdList, InitIrqSources, OwnerInitInput, OwnerInitSchedule};

#[test]
fn owner_input_distinguishes_irq_evidence_from_deadline_activation() {
    let deadline = OwnerInitInput::at(41);
    assert_eq!(deadline.now_ns, 41);
    assert_eq!(deadline.event, None);

    let event = Event {
        tx_queue: IdList::none(),
        rx_queue: IdList::none(),
        device_status: 7,
    };
    let irq = OwnerInitInput::with_event(42, event);
    assert_eq!(irq.now_ns, 42);
    assert_eq!(irq.event, Some(event));
}

#[test]
fn owner_schedule_keeps_irq_and_deadline_as_independent_triggers() {
    let schedule = OwnerInitSchedule::wait_for_irq_until(InitIrqSources::from_bits(0b101), 123_000);
    assert!(!schedule.run_again);
    assert_eq!(schedule.irq_sources.bits(), 0b101);
    assert_eq!(schedule.wake_at_ns, Some(123_000));

    let memory = OwnerInitSchedule::run_again();
    assert!(memory.run_again);
    assert!(memory.irq_sources.is_empty());
    assert_eq!(memory.wake_at_ns, None);
}

#[test]
fn pending_schedule_must_name_a_future_activation() {
    assert!(OwnerInitSchedule::default().validate().is_err());
    assert!(OwnerInitSchedule::run_again().validate().is_ok());
    assert!(
        OwnerInitSchedule::wait_for_irq(InitIrqSources::from_bits(1))
            .validate()
            .is_ok()
    );
    assert!(OwnerInitSchedule::wait_until(1).validate().is_ok());
}
