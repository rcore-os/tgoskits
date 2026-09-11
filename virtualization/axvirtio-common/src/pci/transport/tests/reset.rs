use core::cell::Cell;
use std::{
    sync::{
        Arc as StdArc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use axdevice_base::{AccessWidth, DeviceError};

use super::{super::*, fixtures::*};
use crate::constants::VIRTIO_STATUS_DEVICE_NEEDS_RESET;

#[test]
fn reset_keeps_status_nonzero_until_completion_is_published() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let transition = transport.record_interrupt(false);
    transport.complete_interrupt_transition(transition, true);

    let reset_transition = transport.reset().expect("test reset should succeed");
    assert_eq!(reset_transition, InterruptTransition::Deassert);
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    transport.complete_interrupt_transition(reset_transition, true);
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    transport.complete_reset();
    assert_eq!(transport.status(), 0);
}

#[test]
fn reset_drain_is_bounded_and_keeps_admission_closed_on_timeout() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let permit = transport
        .activity
        .acquire(transport.queue_generation())
        .expect("activity should be admitted");

    assert!(matches!(
        transport.reset(),
        Err(DeviceError::InvalidState { .. })
    ));
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    assert!(!transport.activity.accepting.load(Ordering::Acquire));
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    assert!(matches!(
        transport.write_mmio_with_dma(DEVICE_STATUS, AccessWidth::Byte, 0x0f, true, &mut memory,),
        Err(DeviceError::InvalidState { .. })
    ));
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    drop(permit);
}

#[test]
fn concurrent_reset_has_one_owner() {
    let entered = StdArc::new(Barrier::new(2));
    let release = StdArc::new(Barrier::new(2));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingResetCore {
            entered: StdArc::clone(&entered),
            release: StdArc::clone(&release),
            reset_calls: StdArc::new(AtomicUsize::new(0)),
            allow_reset: None,
        })
        .expect("valid test transport"),
    );
    let first_transport = StdArc::clone(&transport);
    let first = thread::spawn(move || first_transport.reset());
    entered.wait();

    assert!(matches!(
        transport.reset(),
        Err(DeviceError::InvalidState { .. })
    ));
    release.wait();
    assert!(first.join().expect("reset thread should finish").is_ok());
}

#[test]
fn reset_rejects_control_accesses_until_core_reset_finishes() {
    let entered = StdArc::new(Barrier::new(2));
    let release = StdArc::new(Barrier::new(2));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingResetCore {
            entered: StdArc::clone(&entered),
            release: StdArc::clone(&release),
            reset_calls: StdArc::new(AtomicUsize::new(0)),
            allow_reset: None,
        })
        .expect("valid test transport"),
    );
    let reset_transport = StdArc::clone(&transport);
    let reset = thread::spawn(move || reset_transport.reset());
    entered.wait();

    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    assert!(matches!(
        transport.write_mmio_with_dma(QUEUE_DESC, AccessWidth::Qword, 0x1000, true, &mut memory,),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(matches!(
        transport.read_mmio(0x300, AccessWidth::Dword),
        Err(DeviceError::InvalidState { .. })
    ));
    assert!(matches!(
        transport.write_mmio_with_dma(0x300, AccessWidth::Dword, 1, true, &mut memory,),
        Err(DeviceError::InvalidState { .. })
    ));

    release.wait();
    assert!(reset.join().expect("reset thread should finish").is_ok());
}

#[test]
fn reset_waits_for_command_interrupt_transition() {
    let allow_reset = StdArc::new(AtomicBool::new(false));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingResetCore {
            entered: StdArc::new(Barrier::new(1)),
            release: StdArc::new(Barrier::new(1)),
            reset_calls: StdArc::new(AtomicUsize::new(0)),
            allow_reset: Some(StdArc::clone(&allow_reset)),
        })
        .expect("valid test transport"),
    );
    let assert_transition = transport.record_interrupt(false);
    transport.complete_interrupt_transition(assert_transition, true);
    let transition = transport
        .set_interrupt_disabled(true)
        .expect("command transition should be admitted");

    let reset_transport = StdArc::clone(&transport);
    let reset = thread::spawn(move || reset_transport.reset());
    while !transport.activity.resetting.load(Ordering::Acquire)
        || transport.activity.accepting.load(Ordering::Acquire)
    {
        thread::yield_now();
    }
    assert!(transport.activity.resetting.load(Ordering::Acquire));
    assert!(!transport.activity.accepting.load(Ordering::Acquire));

    transport.complete_interrupt_transition(transition.transition(), true);
    allow_reset.store(true, Ordering::Release);
    drop(transition);
    assert!(reset.join().expect("reset thread should finish").is_ok());
}

#[test]
fn command_disable_commits_before_reset_closes_activity() {
    let allow_reset = StdArc::new(AtomicBool::new(false));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingResetCore {
            entered: StdArc::new(Barrier::new(1)),
            release: StdArc::new(Barrier::new(1)),
            reset_calls: StdArc::new(AtomicUsize::new(0)),
            allow_reset: Some(StdArc::clone(&allow_reset)),
        })
        .expect("valid test transport"),
    );
    let entered = StdArc::new(Barrier::new(2));
    let release = StdArc::new(Barrier::new(2));
    let hook_entered = StdArc::clone(&entered);
    let hook_release = StdArc::clone(&release);
    transport.set_reset_before_core_hook(move || {
        hook_entered.wait();
        hook_release.wait();
    });
    let reset_transport = StdArc::clone(&transport);
    let reset = thread::spawn(move || reset_transport.reset());
    entered.wait();

    assert!(matches!(
        transport.set_interrupt_disabled(true),
        Err(DeviceError::InvalidState { .. })
    ));
    allow_reset.store(true, Ordering::Release);
    release.wait();
    let reset_transition = reset.join().expect("reset thread should finish").unwrap();
    assert_eq!(reset_transition, InterruptTransition::None);
    transport.complete_reset();

    // The failed physical synchronization did not roll back the logical
    // INTx-disable command. A completion therefore records ISR state without
    // requesting an assertion; re-enabling later produces the pending assert.
    assert_eq!(transport.record_interrupt(false), InterruptTransition::None);
    let transition = transport
        .set_interrupt_disabled(false)
        .expect("control activity should be reopened after reset");
    assert_eq!(transition.transition(), InterruptTransition::Assert);
    transport.complete_interrupt_transition(transition.transition(), true);
}

#[test]
fn stale_command_transition_is_suppressed_after_reset() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let disabled = transport.update_interrupt_disabled_logical(true);
    assert_eq!(disabled.transition(), InterruptTransition::None);
    assert!(!transport.interrupt_pending());

    assert_eq!(transport.record_interrupt(false), InterruptTransition::None);
    let intent = transport.update_interrupt_disabled_logical(false);
    assert_eq!(intent.transition(), InterruptTransition::Assert);

    assert_eq!(
        transport.reset().expect("reset should succeed"),
        InterruptTransition::None
    );
    transport.complete_reset();

    assert!(
        transport
            .admit_interrupt_transition(intent)
            .expect("stale intent admission should be classified")
            .is_none()
    );
    assert!(!transport.interrupt_pending());
    assert!(!transport.interrupts.asserted());
    assert!(!transport.interrupts.needs_resync());
}

#[test]
fn reset_waits_for_isr_read_transition() {
    let allow_reset = StdArc::new(AtomicBool::new(false));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingResetCore {
            entered: StdArc::new(Barrier::new(1)),
            release: StdArc::new(Barrier::new(1)),
            reset_calls: StdArc::new(AtomicUsize::new(0)),
            allow_reset: Some(StdArc::clone(&allow_reset)),
        })
        .expect("valid test transport"),
    );
    let assert_transition = transport.record_interrupt(false);
    transport.complete_interrupt_transition(assert_transition, true);
    let (value, transition) = transport
        .read_bar_with_interrupt(ISR_CONFIG_OFFSET, AccessWidth::Byte)
        .expect("ISR read should succeed");
    assert_eq!(value, 1);

    let reset_transport = StdArc::clone(&transport);
    let reset = thread::spawn(move || reset_transport.reset());
    while !transport.activity.resetting.load(Ordering::Acquire)
        || transport.activity.accepting.load(Ordering::Acquire)
    {
        thread::yield_now();
    }
    assert!(transport.activity.resetting.load(Ordering::Acquire));
    assert!(!transport.activity.accepting.load(Ordering::Acquire));

    transport.complete_interrupt_transition(transition.transition(), true);
    allow_reset.store(true, Ordering::Release);
    drop(transition);
    assert!(reset.join().expect("reset thread should finish").is_ok());
}
