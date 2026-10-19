//! Receive bursts through the runtime-owned interrupt console, including a
//! second transaction after the first subscription has been drained.

use ax_std::os::arceos::modules::ax_runtime::{console, serial};

pub fn run() -> crate::TestResult {
    let runtime = serial::runtimes()
        .iter()
        .find(|runtime| console::is_active(runtime))
        .expect("serial RX regression requires an active runtime console");
    assert!(
        runtime.info().irq.is_some(),
        "serial RX requires a hardware IRQ"
    );
    #[cfg(target_arch = "aarch64")]
    assert_eq!(runtime.info().name, "PL011 UART");
    let input = console::take_input().expect("take exclusive console input");
    for (phase, expected) in [
        (
            1,
            b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?".as_slice(),
        ),
        (2, b"second-input-after-first-burst".as_slice()),
    ] {
        println!("SERIAL_RX_READY_{phase}");
        let mut offset = 0;
        let mut items = [serial::RxItem::default(); 16];
        while offset < expected.len() {
            let count = input.read(&mut items).expect("blocking console receive");
            for item in &items[..count] {
                assert!(offset < expected.len(), "unexpected extra serial input");
                // Checking the complete item also rejects UART error flags.
                let mut wanted = serial::RxItem::default();
                if let serial::RxItem::Byte { byte, .. } = &mut wanted {
                    *byte = expected[offset];
                }
                assert_eq!(*item, wanted, "serial phase {phase}, byte {offset}");
                offset += 1;
            }
        }
        assert_eq!(
            input.try_read(&mut items),
            0,
            "extra bytes after phase {phase}"
        );
        println!("SERIAL_RX_COMPLETE_{phase}");
    }
    deferred_diagnostic_without_serial_event(runtime);
    Ok(())
}

fn deferred_diagnostic_without_serial_event(runtime: &serial::SerialRuntimeHandle) {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use ax_std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};

    // The QEMU console owner is CPU 0. Keep the observer on that CPU so its
    // sleeps do not send an unrelated wake IPI to the sleeping serial worker.
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    // Drain in two stages: wait_idle pauses new log extraction, so first let
    // the real worker take every prior println from its mailbox. Its drain
    // then covers the current record and the hardware FIFO. No more ordinary
    // output is published before observing the deferred record's consumption.
    while !runtime.log_mailbox_empty_for_test() {
        assert!(
            Instant::now() < deadline,
            "prior serial logs did not leave the mailbox"
        );
        thread::sleep(Duration::from_millis(1));
    }
    runtime
        .task_output()
        .wait_idle()
        .expect("drain prior console output");
    loop {
        if let Some(status) = runtime.publish_deferred_when_blocked_for_test() {
            assert!(status, "deferred diagnostic mailbox rejected the record");
            break;
        }
        assert!(Instant::now() < deadline, "serial worker did not block");
        thread::sleep(Duration::from_millis(1));
    }
    loop {
        if let Some(timed_out) = runtime.deferred_record_consumed_after_timeout_for_test() {
            assert!(
                timed_out,
                "deferred record was consumed by an unrelated wake instead of timeout"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deferred record was not consumed without an IRQ wake"
        );
        thread::sleep(Duration::from_millis(1));
    }
    println!("SERIAL_DEFERRED_DIAGNOSTIC_OK");
}
