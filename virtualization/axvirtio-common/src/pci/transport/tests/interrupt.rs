use core::cell::Cell;

use axdevice_base::{AccessWidth, DeviceError};

use super::{super::*, fixtures::*};
use crate::constants::VIRTIO_STATUS_DEVICE_NEEDS_RESET;

#[test]
fn reset_retries_deassert_after_failed_assert_transition() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let assert_transition = transport.record_interrupt(true);
    assert_eq!(assert_transition, InterruptTransition::Assert);
    transport.complete_interrupt_transition(assert_transition, false);

    let reset_transition = transport.reset().expect("reset should retry line cleanup");
    assert_eq!(reset_transition, InterruptTransition::Deassert);
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    transport.complete_interrupt_transition(reset_transition, true);
    transport.complete_reset();
    assert_eq!(transport.status(), 0);
}

#[test]
fn dropping_interrupt_transition_request_keeps_transition_retryable() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let assert_transition = transport.record_interrupt(false);
    transport.complete_interrupt_transition(assert_transition, true);

    let request = transport
        .set_interrupt_disabled(true)
        .expect("disabling interrupts should return a transition request");
    assert_eq!(request.transition(), InterruptTransition::Deassert);
    drop(request);

    let retry = transport
        .set_interrupt_disabled(true)
        .expect("retrying the disabled state should return a request");
    assert_eq!(retry.transition(), InterruptTransition::Deassert);
    transport.complete_interrupt_transition(retry.transition(), true);
    drop(retry);
}

#[test]
fn stale_endpoint_irq_suppression_does_not_create_retry_state() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let transition = transport.record_interrupt(false);
    assert_eq!(transition, InterruptTransition::Assert);
    assert!(!transport.interrupts.needs_resync());

    // A closed endpoint IRQ admission suppresses the transition before any
    // ISR publication or physical-line operation. It must not turn the
    // intentionally discarded effect into a new retry request.
    transport.suppress_stale_interrupt_transition(transition);
    assert!(!transport.interrupts.needs_resync());
}

#[test]
fn isr_read_is_read_to_clear() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let transition = transport.record_interrupt(false);
    assert_eq!(transition, InterruptTransition::Assert);
    transport.complete_interrupt_transition(transition, true);
    assert!(matches!(
        transport.read_bar(ISR_CONFIG_OFFSET, AccessWidth::Byte),
        Err(DeviceError::Unsupported { .. })
    ));
    let (value, request) = transport
        .read_bar_with_interrupt(ISR_CONFIG_OFFSET, AccessWidth::Byte)
        .expect("ISR read should succeed");
    assert_eq!(value, 1);
    transport.complete_interrupt_transition(request.transition(), true);
    drop(request);
    let (value, request) = transport
        .read_bar_with_interrupt(ISR_CONFIG_OFFSET, AccessWidth::Byte)
        .expect("second ISR read should succeed");
    assert_eq!(value, 0);
    drop(request);
}

#[test]
fn queue_notification_keeps_activity_until_irq_publication() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let interrupts = Arc::new(VirtioPciInterruptCoordinator::new());
    let activity = Arc::new(QueueActivity::new());
    let permit = activity
        .acquire(transport.queue_generation())
        .expect("activity should be admitted");
    let notification = QueueNotification {
        outcome: QueueNotifyOutcome::Completed { notify: true },
        publication: InterruptPublicationRequest::new(
            Arc::clone(&interrupts),
            Some(InterruptPublicationKind::Queue),
            Some(permit),
        ),
    };
    assert!(!interrupts.pending());
    let published = Cell::new(false);
    notification
        .publish(|actual| {
            assert_eq!(actual, InterruptTransition::Assert);
            published.set(true);
            Ok(())
        })
        .expect("queue IRQ publication should succeed");
    assert!(published.get());
    assert!(!interrupts.needs_resync());
    activity.close_and_drain();
}
