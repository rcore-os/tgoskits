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
    Ok(())
}
