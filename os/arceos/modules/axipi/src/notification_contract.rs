use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_hal::irq::{CpuId, IrqError};

use crate::{IpiNotification, notification::DeliveryEdges};

#[test]
fn first_publication_sends_and_repeated_publication_coalesces() {
    let edges = DeliveryEdges::<2>::new();
    let sends = AtomicUsize::new(0);

    assert_eq!(
        edges.notify(CpuId(1), || {
            sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        Ok(IpiNotification::Sent),
    );
    assert_eq!(
        edges.notify(CpuId(1), || {
            sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        Ok(IpiNotification::Coalesced),
    );
    assert_eq!(sends.load(Ordering::Relaxed), 1);
}

#[test]
fn publication_after_claim_obtains_a_fresh_edge() {
    let edges = DeliveryEdges::<2>::new();
    let sends = AtomicUsize::new(0);

    assert_eq!(
        edges.notify(CpuId(1), || {
            sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        Ok(IpiNotification::Sent),
    );
    edges.claim(CpuId(1));
    assert_eq!(
        edges.notify(CpuId(1), || {
            sends.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }),
        Ok(IpiNotification::Sent),
    );
    assert_eq!(sends.load(Ordering::Relaxed), 2);
}

#[test]
fn claim_during_controller_send_cannot_overwrite_a_fresh_edge() {
    let edges = DeliveryEdges::<2>::new();
    let sends = AtomicUsize::new(0);

    assert_eq!(
        edges.notify(CpuId(1), || {
            sends.fetch_add(1, Ordering::Relaxed);
            edges.claim(CpuId(1));
            assert_eq!(
                edges.notify(CpuId(1), || {
                    sends.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }),
                Ok(IpiNotification::Sent),
            );
            Ok(())
        }),
        Ok(IpiNotification::Sent),
    );
    assert_eq!(sends.load(Ordering::Relaxed), 2);
    assert_eq!(
        edges.notify(CpuId(1), || panic!("fresh edge must remain armed")),
        Ok(IpiNotification::Coalesced),
    );
}

#[test]
fn delivery_failure_is_reported_and_does_not_consume_owner_pending() {
    let edges = DeliveryEdges::<2>::new();
    let owner_pending = AtomicBool::new(false);

    owner_pending.store(true, Ordering::Release);
    assert_eq!(
        edges.notify(CpuId(1), || Err(IrqError::Controller)),
        Err(IrqError::Controller),
    );
    assert!(owner_pending.load(Ordering::Acquire));

    assert_eq!(edges.notify(CpuId(1), || Ok(())), Ok(IpiNotification::Sent),);
}

#[test]
fn invalid_target_is_rejected_before_delivery() {
    let edges = DeliveryEdges::<1>::new();

    assert_eq!(
        edges.notify(CpuId(1), || panic!("invalid target must not be sent")),
        Err(IrqError::InvalidCpu),
    );
}
