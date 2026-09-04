use core::cell::Cell;
use std::{
    sync::{
        Arc as StdArc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use axdevice_base::{AccessWidth, DeviceError};

use super::{super::*, fixtures::*};
use crate::VIRTIO_STATUS_DEVICE_NEEDS_RESET;

#[test]
fn failed_status_stops_queue_processing() {
    let notify_calls = StdArc::new(AtomicUsize::new(0));
    let transport = VirtioPciTransport::try_new(CountingNotifyCore {
        notify_calls: StdArc::clone(&notify_calls),
    })
    .expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut memory);
    for status in [0x0b, 0x0f] {
        write(
            &transport,
            DEVICE_STATUS,
            AccessWidth::Byte,
            status,
            &mut memory,
        );
    }
    for (offset, width, value) in [
        (QUEUE_DESC, AccessWidth::Qword, 0x1000),
        (QUEUE_DRIVER, AccessWidth::Qword, 0x2000),
        (QUEUE_DEVICE, AccessWidth::Qword, 0x3000),
        (QUEUE_ENABLE, AccessWidth::Word, 1),
    ] {
        write(&transport, offset, width, value, &mut memory);
    }

    let first = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        )
        .expect("running queue notification should succeed");
    let VirtioPciWriteOutcome::QueueNotified(first) = first else {
        panic!("expected queue notification");
    };
    first.complete();
    assert_eq!(notify_calls.load(Ordering::Acquire), 1);

    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x8f,
        &mut memory,
    );
    let stopped = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        )
        .expect("failed driver status should stop the queue without faulting");
    let VirtioPciWriteOutcome::QueueNotified(stopped) = stopped else {
        panic!("expected stopped queue notification");
    };
    assert_eq!(stopped.outcome(), QueueNotifyOutcome::Idle);
    stopped.complete();
    assert_eq!(notify_calls.load(Ordering::Acquire), 1);
}

#[test]
fn queue_notify_probes_the_programmed_ring() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut memory = TestMemory {
        reads: Cell::new(0),
    };

    acknowledge_driver(&transport, &mut memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut memory,
    );
    write(&transport, QUEUE_ENABLE, AccessWidth::Word, 1, &mut memory);
    let outcome = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        )
        .expect("queue notify should succeed");

    let VirtioPciWriteOutcome::QueueNotified(notification) = outcome else {
        panic!("expected queue notification");
    };
    assert_eq!(notification.outcome(), QueueNotifyOutcome::Idle);
    notification.complete();
    assert_eq!(memory.reads.get(), 6);
}

#[test]
fn admitted_queue_fault_sets_needs_reset_and_config_isr_once() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut configuration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut configuration_memory,
    );

    let mut failing_memory = FailingMemory;
    let outcome = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut failing_memory,
        )
        .expect("queue faults are reported as a transport outcome");
    let VirtioPciWriteOutcome::Fault { publication, .. } = outcome else {
        panic!("expected a queue fault outcome");
    };
    assert!(publication.requires_irq_permit());
    assert_ne!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );
    publication
        .publish(|transition| {
            assert_eq!(transition, InterruptTransition::Assert);
            Ok(())
        })
        .expect("config-change IRQ publication should succeed");
    let (value, request) = transport
        .read_bar_with_interrupt(ISR_CONFIG_OFFSET, AccessWidth::Byte)
        .expect("ISR read should succeed");
    assert_eq!(value, 2);
    transport.complete_interrupt_transition(request.transition(), true);
    drop(request);
}

#[test]
fn stale_fault_publication_does_not_leave_config_isr_pending() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    for status in [0x0b, 0x0f] {
        write(
            &transport,
            DEVICE_STATUS,
            AccessWidth::Byte,
            status,
            &mut configuration_memory,
        );
    }
    for (offset, width, value) in [
        (QUEUE_DESC, AccessWidth::Qword, 0x1000),
        (QUEUE_DRIVER, AccessWidth::Qword, 0x2000),
        (QUEUE_DEVICE, AccessWidth::Qword, 0x3000),
        (QUEUE_ENABLE, AccessWidth::Word, 1),
    ] {
        write(&transport, offset, width, value, &mut configuration_memory);
    }

    let mut failing_memory = FailingMemory;
    let outcome = transport
        .write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut failing_memory,
        )
        .expect("queue faults are returned as a transport outcome");
    let VirtioPciWriteOutcome::Fault { publication, .. } = outcome else {
        panic!("expected a queue fault");
    };

    publication.cancel();
    assert!(!transport.interrupt_pending());
}

#[test]
fn queue_fault_activity_blocks_reset_until_terminal_publication() {
    let transport = VirtioPciTransport::try_new(TestCore).expect("valid test transport");
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut configuration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut configuration_memory,
    );

    let transport = StdArc::new(transport);
    let fault_transport = StdArc::clone(&transport);
    let (sender, receiver) = mpsc::channel();
    let fault_thread = thread::spawn(move || {
        let mut failing_memory = FailingMemory;
        let outcome = fault_transport
            .write_mmio_with_dma(
                NOTIFY_CONFIG_OFFSET,
                AccessWidth::Word,
                0,
                true,
                &mut failing_memory,
            )
            .expect("queue faults are returned as an outcome");
        sender
            .send(outcome)
            .expect("queue fault should be delivered");
    });
    let outcome = receiver
        .recv()
        .expect("queue fault should be available before reset");
    fault_thread.join().expect("fault thread should finish");
    let VirtioPciWriteOutcome::Fault { publication, .. } = outcome else {
        panic!("expected a queue fault");
    };

    let reset_transport = StdArc::clone(&transport);
    let reset_thread = thread::spawn(move || reset_transport.reset());
    assert!(matches!(
        reset_thread.join().expect("reset thread should finish"),
        Err(DeviceError::InvalidState { .. })
    ));
    publication
        .publish(|transition| {
            assert_eq!(transition, InterruptTransition::Assert);
            Ok(())
        })
        .expect("fault publication should succeed");

    let reset_transition = transport
        .reset()
        .expect("reset should proceed after fault publication");
    transport.complete_interrupt_transition(reset_transition, true);
    transport.complete_reset();
    assert_eq!(transport.status(), 0);
}

#[test]
fn concurrent_notify_same_queue_does_not_fault_or_replace_owner_queue() {
    let entered = StdArc::new(Barrier::new(2));
    let release = StdArc::new(Barrier::new(2));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(BlockingNotifyCore {
            entered: StdArc::clone(&entered),
            release: StdArc::clone(&release),
        })
        .expect("valid test transport"),
    );
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut configuration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut configuration_memory,
    );

    let first_transport = StdArc::clone(&transport);
    let (sender, receiver) = mpsc::channel();
    let first = thread::spawn(move || {
        let mut memory = TestMemory {
            reads: Cell::new(0),
        };
        let result = first_transport.write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        );
        sender
            .send(result)
            .expect("first notify result should be delivered");
    });
    entered.wait();

    let second_transport = StdArc::clone(&transport);
    let second = thread::spawn(move || {
        let mut memory = TestMemory {
            reads: Cell::new(0),
        };
        second_transport.write_mmio_with_dma(
            NOTIFY_CONFIG_OFFSET,
            AccessWidth::Word,
            0,
            true,
            &mut memory,
        )
    });
    let second_result = second.join().expect("second notify should finish");
    let second_outcome = second_result.expect("second notify should not fail");
    let VirtioPciWriteOutcome::QueueNotified(notification) = second_outcome else {
        panic!("expected an idle queue notification");
    };
    assert_eq!(notification.outcome(), QueueNotifyOutcome::Idle);
    notification.complete();
    assert_eq!(
        transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8,
        0
    );

    release.wait();
    let first_outcome = receiver
        .recv()
        .expect("first notify result should be available")
        .expect("first notify should succeed");
    let VirtioPciWriteOutcome::QueueNotified(notification) = first_outcome else {
        panic!("expected first queue notification");
    };
    notification.complete();
    first.join().expect("first notify should finish");
}

#[test]
fn stale_queue_notification_does_not_process_reconfigured_generation() {
    let notify_calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
    let transport = StdArc::new(
        VirtioPciTransport::try_new(CountingNotifyCore {
            notify_calls: StdArc::clone(&notify_calls),
        })
        .expect("valid test transport"),
    );
    let mut configuration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut configuration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut configuration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x1000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x2000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x3000,
        &mut configuration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut configuration_memory,
    );

    let snapshot_taken = StdArc::new(Barrier::new(2));
    let release_snapshot = StdArc::new(Barrier::new(2));
    transport.set_notify_admission_hook({
        let snapshot_taken = StdArc::clone(&snapshot_taken);
        let release_snapshot = StdArc::clone(&release_snapshot);
        move || {
            snapshot_taken.wait();
            release_snapshot.wait();
        }
    });

    let notify_transport = StdArc::clone(&transport);
    let notify_thread = thread::spawn(move || {
        let mut memory = TestMemory {
            reads: Cell::new(0),
        };
        notify_transport
            .write_mmio_with_dma(
                NOTIFY_CONFIG_OFFSET,
                AccessWidth::Word,
                0,
                true,
                &mut memory,
            )
            .expect("stale queue notify should be safely suppressed")
    });
    snapshot_taken.wait();

    let old_generation = transport.queue_generation();
    let reset_transition = transport
        .reset()
        .expect("reset should proceed before admission");
    transport.complete_interrupt_transition(reset_transition, true);
    transport.complete_reset();
    assert_ne!(transport.queue_generation(), old_generation);

    // Reopen a new, valid queue generation before allowing the old notify to
    // acquire activity.  Generation validation, rather than queue.enabled,
    // must reject the old operation here.
    let mut reconfiguration_memory = TestMemory {
        reads: Cell::new(0),
    };
    acknowledge_driver(&transport, &mut reconfiguration_memory);
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0b,
        &mut reconfiguration_memory,
    );
    write(
        &transport,
        DEVICE_STATUS,
        AccessWidth::Byte,
        0x0f,
        &mut reconfiguration_memory,
    );
    write(
        &transport,
        QUEUE_DESC,
        AccessWidth::Qword,
        0x4000,
        &mut reconfiguration_memory,
    );
    write(
        &transport,
        QUEUE_DRIVER,
        AccessWidth::Qword,
        0x5000,
        &mut reconfiguration_memory,
    );
    write(
        &transport,
        QUEUE_DEVICE,
        AccessWidth::Qword,
        0x6000,
        &mut reconfiguration_memory,
    );
    write(
        &transport,
        QUEUE_ENABLE,
        AccessWidth::Word,
        1,
        &mut reconfiguration_memory,
    );

    release_snapshot.wait();
    let outcome = notify_thread
        .join()
        .expect("stale notification thread should finish");
    let VirtioPciWriteOutcome::QueueNotified(notification) = outcome else {
        panic!("expected stale queue notification");
    };
    assert_eq!(notification.outcome(), QueueNotifyOutcome::Idle);
    notification.complete();
    assert_eq!(
        notify_calls.load(std::sync::atomic::Ordering::Acquire),
        0,
        "a notification from the old queue generation must not enter the core"
    );
}
