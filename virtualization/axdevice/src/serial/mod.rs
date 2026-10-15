//! Reusable virtual serial devices.

mod backend;
mod device;
mod endpoint;
mod fifo;
mod pl011;
mod uart16550;

pub use backend::{
    NullSerialBackend, NullSerialBackendFactory, SerialBackend, SerialBackendFactory,
};
pub use device::{build_16550_mmio, build_16550_port, build_pl011_mmio};
use endpoint::SerialEndpoint;
pub use pl011::Pl011;
pub use uart16550::Uart16550;

#[cfg(test)]
mod tests {
    use alloc::{
        collections::VecDeque,
        sync::{Arc, Weak},
        vec,
        vec::Vec,
    };
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Barrier, Mutex, mpsc};

    use axdevice_base::{
        AccessWidth, ControllerInputId, InterruptControllerId, InterruptTriggerMode, IrqLine,
        IrqResult, WiredIrqInput, WiredIrqSink,
    };

    use super::{Pl011, SerialBackend, Uart16550};
    use crate::host_lock_provider::NEXT_MUTEX_WAIT;

    #[derive(Debug, Default)]
    struct TestBackend {
        input: Mutex<VecDeque<u8>>,
        output: Mutex<Vec<u8>>,
        blocked: AtomicBool,
    }

    impl TestBackend {
        fn push_input(&self, bytes: &[u8]) {
            self.input.lock().unwrap().extend(bytes);
        }

        fn set_blocked(&self, blocked: bool) {
            self.blocked.store(blocked, Ordering::Release);
        }
    }

    impl SerialBackend for TestBackend {
        fn write(&self, bytes: &[u8]) {
            self.output.lock().unwrap().extend_from_slice(bytes);
        }

        fn try_write(&self, bytes: &[u8]) -> usize {
            if self.blocked.load(Ordering::Acquire) {
                return 0;
            }
            self.write(bytes);
            bytes.len()
        }

        fn read(&self, buffer: &mut [u8]) -> usize {
            let mut input = self.input.lock().unwrap();
            let count = buffer.len().min(input.len());
            for target in &mut buffer[..count] {
                *target = input.pop_front().unwrap();
            }
            count
        }
    }

    #[derive(Default)]
    struct TestIrqSink {
        levels: Mutex<Vec<bool>>,
    }

    impl WiredIrqSink for TestIrqSink {
        fn set_level(&self, _input: ControllerInputId, asserted: bool) -> IrqResult {
            self.levels.lock().unwrap().push(asserted);
            Ok(())
        }

        fn pulse(&self, _input: ControllerInputId) -> IrqResult {
            Ok(())
        }
    }

    fn level_irq(sink: Arc<TestIrqSink>, line: usize) -> IrqLine {
        WiredIrqInput::new(
            InterruptControllerId::new(0),
            ControllerInputId::new(line),
            InterruptTriggerMode::LevelTriggered,
            sink,
        )
        .connect()
        .unwrap()
    }

    #[test]
    fn uart16550_routes_tx_rx_fifo_and_level_irq() {
        let backend = Arc::new(TestBackend::default());
        let sink = Arc::new(TestIrqSink::default());
        let uart = Uart16550::new(backend.clone(), level_irq(sink.clone(), 4));

        uart.write(1, AccessWidth::Byte, 1).unwrap();
        backend.push_input(b"ab");
        uart.poll().unwrap();
        uart.poll().unwrap();
        assert_eq!(
            sink.levels.lock().unwrap().as_slice(),
            [true],
            "the controller input observes electrical transitions, not repeated level samples"
        );
        assert_eq!(uart.read(5, AccessWidth::Byte).unwrap() & 1, 1);
        assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b'a' as u64);
        assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b'b' as u64);
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));

        uart.write(0, AccessWidth::Byte, b'Z' as u64).unwrap();
        assert_eq!(backend.output.lock().unwrap().as_slice(), b"Z");

        for (length, polls) in [(73, 1), (529, 9)] {
            let input: Vec<_> = (0..length).map(|index| (index % 251) as u8).collect();
            backend.push_input(&input);
            // Extra work notifications must leave bytes in the backend once
            // the FIFO is full, even if the guest has not consumed anything.
            for _ in 0..polls {
                uart.poll().unwrap();
            }
            let mut received = Vec::new();
            for _ in 0..=length {
                let status = uart.read(5, AccessWidth::Byte).unwrap();
                assert_eq!(status & 2, 0, "backend input must not overrun the FIFO");
                if status & 1 == 0 {
                    break;
                }
                received.push(uart.read(0, AccessWidth::Byte).unwrap() as u8);
            }
            assert_eq!(
                received, input,
                "RX must progress without another device poll"
            );
            assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));
        }
    }

    #[test]
    fn uart16550_mask_and_fifo_clear_deassert_irq() {
        let backend = Arc::new(TestBackend::default());
        let sink = Arc::new(TestIrqSink::default());
        let uart = Uart16550::new(backend.clone(), level_irq(sink.clone(), 4));

        backend.push_input(b"x");
        uart.poll().unwrap();
        assert!(
            sink.levels.lock().unwrap().is_empty(),
            "a source that starts deasserted must not emit a redundant low transition"
        );

        uart.write(1, AccessWidth::Byte, 1).unwrap();
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&true));
        uart.write(2, AccessWidth::Byte, 1 << 1).unwrap();
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));
        assert_eq!(sink.levels.lock().unwrap().as_slice(), [true, false]);

        let input: Vec<_> = (0..529).map(|index| (index % 251) as u8).collect();
        backend.push_input(&input);
        for _ in 0..9 {
            uart.poll().unwrap();
        }
        uart.write(2, AccessWidth::Byte, 1 << 1).unwrap();
        let mut received = Vec::new();
        for _ in 0..=input.len() {
            if uart.read(5, AccessWidth::Byte).unwrap() & 1 == 0 {
                break;
            }
            received.push(uart.read(0, AccessWidth::Byte).unwrap() as u8);
        }
        assert_eq!(
            received,
            input[256..],
            "reset discards only committed FIFO bytes"
        );
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));
    }

    #[test]
    fn uart16550_thr_write_rearms_empty_irq_after_tx_completion() {
        let backend = Arc::new(TestBackend::default());
        let sink = Arc::new(TestIrqSink::default());
        let uart = Uart16550::new(backend.clone(), level_irq(sink.clone(), 4));

        uart.write(1, AccessWidth::Byte, 1 << 1).unwrap();
        assert_eq!(sink.levels.lock().unwrap().as_slice(), [true]);

        uart.write(0, AccessWidth::Byte, b'P' as u64).unwrap();
        assert_eq!(backend.output.lock().unwrap().as_slice(), b"P");
        let levels_after_write = sink.levels.lock().unwrap().clone();
        assert_eq!(
            levels_after_write,
            [true, false, true],
            "synchronous TX must consume and then rearm THRE for an edge-routed ISA IRQ"
        );

        uart.poll().unwrap();
        let levels_after_completion = sink.levels.lock().unwrap().clone();
        assert_eq!(
            levels_after_completion,
            [true, false, true],
            "TX completion must not depend on an unrelated backend input poll"
        );
    }

    #[test]
    fn pl011_exposes_ids_fifo_and_masked_level_irq() {
        let backend = Arc::new(TestBackend::default());
        let sink = Arc::new(TestIrqSink::default());
        let uart = Pl011::new(backend.clone(), level_irq(sink.clone(), 33));

        assert_eq!(uart.read(0xfe0, AccessWidth::Dword).unwrap(), 0x11);
        uart.write(0x038, AccessWidth::Word, 1 << 4).unwrap();
        assert_eq!(uart.read(0x038, AccessWidth::Word).unwrap(), 1 << 4);
        backend.push_input(b"q");
        uart.poll().unwrap();
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&true));
        assert_eq!(uart.read(0x000, AccessWidth::Dword).unwrap(), b'q' as u64);
        assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));

        uart.write(0x000, AccessWidth::Dword, b'P' as u64).unwrap();
        assert_eq!(backend.output.lock().unwrap().as_slice(), b"P");

        for (length, polls) in [(73, 1), (529, 9)] {
            let input: Vec<_> = (0..length).map(|index| (index % 251) as u8).collect();
            backend.push_input(&input);
            for _ in 0..polls {
                uart.poll().unwrap();
            }
            let mut received = Vec::new();
            for _ in 0..=length {
                if uart.read(0x018, AccessWidth::Dword).unwrap() & (1 << 4) != 0 {
                    break;
                }
                let value = uart.read(0x000, AccessWidth::Dword).unwrap();
                assert_eq!(value & !0xff, 0, "backend input must not overrun the FIFO");
                received.push(value as u8);
            }
            assert_eq!(
                received, input,
                "RX must progress without another device poll"
            );
            assert_eq!(sink.levels.lock().unwrap().last(), Some(&false));
        }
    }

    #[derive(Debug)]
    struct PausedReadBackend {
        bytes: TestBackend,
        pause_next: AtomicBool,
        panic_next: AtomicBool,
        entered: Barrier,
        release: Barrier,
        reset_and_loopback: Mutex<Option<Weak<Uart16550>>>,
    }

    impl PausedReadBackend {
        fn new() -> Self {
            Self {
                bytes: TestBackend::default(),
                pause_next: AtomicBool::new(false),
                panic_next: AtomicBool::new(false),
                entered: Barrier::new(2),
                release: Barrier::new(2),
                reset_and_loopback: Mutex::new(None),
            }
        }
    }

    impl SerialBackend for PausedReadBackend {
        fn write(&self, bytes: &[u8]) {
            self.bytes.write(bytes);
        }

        fn read(&self, buffer: &mut [u8]) -> usize {
            assert!(
                !self.panic_next.swap(false, Ordering::AcqRel),
                "injected RX unwind"
            );
            let count = self.bytes.read(buffer);
            let callback = self.reset_and_loopback.lock().unwrap().take();
            if let Some(uart) = callback.and_then(|uart| uart.upgrade()) {
                uart.write(2, AccessWidth::Byte, 1 << 1).unwrap();
                uart.write(4, AccessWidth::Byte, 1 << 4).unwrap();
                uart.write(0, AccessWidth::Byte, b'C' as u64).unwrap();
                // Emptying the FIFO reenters refill while this backend read
                // owns the RX gate. It must return without waiting on itself.
                assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b'C' as u64);
            }
            if self.pause_next.swap(false, Ordering::AcqRel) {
                self.entered.wait();
                self.release.wait();
            }
            count
        }
    }

    struct MutexWaitObserver;

    impl MutexWaitObserver {
        fn new(events: mpsc::Sender<bool>) -> Self {
            NEXT_MUTEX_WAIT.with(|slot| assert!(slot.borrow_mut().replace(events).is_none()));
            Self
        }
    }

    impl Drop for MutexWaitObserver {
        fn drop(&mut self) {
            NEXT_MUTEX_WAIT.with(|slot| *slot.borrow_mut() = None);
        }
    }

    #[test]
    fn uart16550_poll_preserves_work_arriving_during_an_empty_read() {
        let backend = Arc::new(PausedReadBackend::new());
        let sink = Arc::new(TestIrqSink::default());
        let uart = Uart16550::new(backend.clone(), level_irq(sink.clone(), 4));
        uart.write(1, AccessWidth::Byte, 1).unwrap();
        backend.pause_next.store(true, Ordering::Release);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| uart.poll());
            // The first read already observed an empty backend, but has not
            // committed. New input and its sole poll now overlap that read.
            backend.entered.wait();
            backend.bytes.push_input(b"n");
            let (events, received) = mpsc::channel();
            let uart = &uart;
            let second = scope.spawn(move || {
                let _observer = MutexWaitObserver::new(events.clone());
                let result = uart.poll();
                events.send(false).unwrap();
                result
            });
            let waited = received.recv().unwrap();
            backend.release.wait();
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
            assert!(
                waited,
                "a notified poll must wait instead of skipping an in-flight read"
            );
        });
        assert_eq!(sink.levels.lock().unwrap().as_slice(), [true]);
        assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b'n' as u64);
        assert_eq!(uart.read(5, AccessWidth::Byte).unwrap() & 3, 0);
        assert_eq!(sink.levels.lock().unwrap().as_slice(), [true, false]);
    }

    #[test]
    fn uart16550_preserves_inflight_rx_capacity_across_reset_and_loopback() {
        let backend = Arc::new(PausedReadBackend::new());
        let uart = Arc::new(Uart16550::new(
            backend.clone(),
            level_irq(Arc::new(TestIrqSink::default()), 4),
        ));
        backend.bytes.push_input(&[b'o'; 192]);
        for _ in 0..3 {
            uart.poll().unwrap();
        }
        let pending: Vec<_> = (0..64).collect();
        backend.bytes.push_input(&pending);
        backend.pause_next.store(true, Ordering::Release);
        std::thread::scope(|scope| {
            let poller = scope.spawn(|| uart.poll());
            backend.entered.wait();
            // Register accesses do not wait for the backend. FIFO reset
            // keeps the first read's reservation, including its capacity.
            uart.write(4, AccessWidth::Byte, 1 << 4).unwrap();
            uart.write(0, AccessWidth::Byte, b'x' as u64).unwrap();
            let full_overrun = uart.read(5, AccessWidth::Byte).unwrap() & 2;
            uart.write(2, AccessWidth::Byte, 1 << 1).unwrap();
            for _ in 0..192 {
                uart.write(0, AccessWidth::Byte, b'L' as u64).unwrap();
            }
            uart.write(0, AccessWidth::Byte, b'x' as u64).unwrap();
            let reset_overrun = uart.read(5, AccessWidth::Byte).unwrap() & 2;
            uart.write(2, AccessWidth::Byte, 1 << 1).unwrap();
            uart.write(0, AccessWidth::Byte, b'A' as u64).unwrap();
            uart.write(0, AccessWidth::Byte, b'B' as u64).unwrap();
            let first = uart.read(0, AccessWidth::Byte).unwrap();
            backend.release.wait();
            poller.join().unwrap().unwrap();
            assert_eq!(full_overrun, 2, "loopback cannot consume reserved RX space");
            assert_eq!(
                reset_overrun, 2,
                "FIFO clear must preserve in-flight capacity"
            );
            assert_eq!(first, b'A' as u64);
        });
        uart.write(4, AccessWidth::Byte, 0).unwrap();
        let mut received = Vec::new();
        for _ in 0..66 {
            if uart.read(5, AccessWidth::Byte).unwrap() & 1 == 0 {
                break;
            }
            received.push(uart.read(0, AccessWidth::Byte).unwrap() as u8);
        }
        let mut expected = vec![b'B'];
        expected.extend_from_slice(&pending);
        assert_eq!(
            received, expected,
            "reset removes old bytes, not the in-flight arrival"
        );

        backend.bytes.push_input(b"s");
        *backend.reset_and_loopback.lock().unwrap() = Some(Arc::downgrade(&uart));
        uart.poll().unwrap();
        uart.write(4, AccessWidth::Byte, 0).unwrap();
        assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b's' as u64);

        backend.bytes.push_input(b"r");
        backend.panic_next.store(true, Ordering::Release);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| uart.poll())).is_err());
        uart.poll().unwrap();
        assert_eq!(uart.read(0, AccessWidth::Byte).unwrap(), b'r' as u64);
        assert_eq!(uart.read(5, AccessWidth::Byte).unwrap() & 3, 0);
    }

    #[test]
    fn pl011_retains_tx_until_the_backend_accepts_it() {
        const FR_BUSY: u64 = 1 << 3;
        const FR_TXFE: u64 = 1 << 7;

        let backend = Arc::new(TestBackend::default());
        backend.set_blocked(true);
        let uart = Pl011::new(
            backend.clone(),
            level_irq(Arc::new(TestIrqSink::default()), 33),
        );

        uart.write(0x000, AccessWidth::Dword, b'P' as u64).unwrap();
        assert!(backend.output.lock().unwrap().is_empty());
        let flags = uart.read(0x018, AccessWidth::Dword).unwrap();
        assert_ne!(flags & FR_BUSY, 0);
        assert_eq!(flags & FR_TXFE, 0);

        backend.set_blocked(false);
        uart.poll().unwrap();
        assert_eq!(backend.output.lock().unwrap().as_slice(), b"P");
        let flags = uart.read(0x018, AccessWidth::Dword).unwrap();
        assert_eq!(flags & FR_BUSY, 0);
        assert_ne!(flags & FR_TXFE, 0);
    }

    #[test]
    fn pl011_retained_tx_waits_for_the_transmitter_to_be_reenabled() {
        const FR_BUSY: u64 = 1 << 3;
        const FR_TXFF: u64 = 1 << 5;
        const FR_TXFE: u64 = 1 << 7;
        const CR_UART_ENABLE: u64 = 1;
        const CR_TX_ENABLE: u64 = 1 << 8;
        const CR_RX_ENABLE: u64 = 1 << 9;

        let active = CR_UART_ENABLE | CR_TX_ENABLE | CR_RX_ENABLE;
        let cases: [(&str, u64); 3] = [
            ("clearing CR_UART_ENABLE", active & !CR_UART_ENABLE),
            ("clearing CR_TX_ENABLE", active & !CR_TX_ENABLE),
            (
                "clearing CR_UART_ENABLE and CR_TX_ENABLE",
                active & !(CR_UART_ENABLE | CR_TX_ENABLE),
            ),
        ];

        for (description, disabled) in cases {
            let backend = Arc::new(TestBackend::default());
            backend.set_blocked(true);
            let uart = Pl011::new(
                backend.clone(),
                level_irq(Arc::new(TestIrqSink::default()), 33),
            );

            uart.write(0x000, AccessWidth::Dword, b'R' as u64).unwrap();
            assert!(
                backend.output.lock().unwrap().is_empty(),
                "{description}: a blocked backend must retain the byte in the TX FIFO"
            );

            uart.write(0x030, AccessWidth::Dword, disabled).unwrap();
            backend.set_blocked(false);

            uart.poll().unwrap();
            assert!(
                backend.output.lock().unwrap().is_empty(),
                "{description}: poll must not submit retained bytes while the transmitter is \
                 disabled"
            );
            let flags = uart.read(0x018, AccessWidth::Dword).unwrap();
            assert_eq!(
                flags & (FR_TXFF | FR_BUSY | FR_TXFE),
                FR_BUSY,
                "{description}: a disabled transmitter must still report retained data as pending"
            );
            assert!(
                backend.output.lock().unwrap().is_empty(),
                "{description}: reading FR must not submit retained bytes while the transmitter \
                 is disabled"
            );

            uart.write(0x030, AccessWidth::Dword, active).unwrap();
            assert_eq!(
                backend.output.lock().unwrap().as_slice(),
                [b'R'],
                "{description}: re-enabling the transmitter must continue the retained byte \
                 exactly once"
            );
            let flags = uart.read(0x018, AccessWidth::Dword).unwrap();
            assert_eq!(
                flags & (FR_TXFF | FR_BUSY | FR_TXFE),
                FR_TXFE,
                "{description}: an enabled transmitter with an empty FIFO is idle and TXFE"
            );
        }
    }
}
