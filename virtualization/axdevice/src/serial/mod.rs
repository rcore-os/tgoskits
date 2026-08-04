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
    use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
    use std::sync::Mutex;

    use axdevice_base::{
        AccessWidth, ControllerInputId, InterruptControllerId, InterruptTriggerMode, IrqLine,
        IrqResult, WiredIrqInput, WiredIrqSink,
    };

    use super::{Pl011, SerialBackend, Uart16550};

    #[derive(Debug, Default)]
    struct TestBackend {
        input: Mutex<VecDeque<u8>>,
        output: Mutex<Vec<u8>>,
    }

    impl TestBackend {
        fn push_input(&self, bytes: &[u8]) {
            self.input.lock().unwrap().extend(bytes);
        }
    }

    impl SerialBackend for TestBackend {
        fn write(&self, bytes: &[u8]) {
            self.output.lock().unwrap().extend_from_slice(bytes);
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
    }
}
