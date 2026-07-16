#![cfg(target_arch = "x86_64")]

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axdevice::{
    AccessWidth, ControllerInputId, Device, InterruptControllerId, InterruptTriggerMode, IrqResult,
    WiredIrqInput, WiredIrqSink, X86SerialBackend, X86SerialDeviceOps, X86SerialPortDevice,
};
use axdevice_base::{BusAccess, BusKind, BusResponse};
use x86_vlapic::{X86AccessWidth, X86Port};

#[test]
fn edge_triggered_com1_pulses_once_for_each_assertion() {
    let sink = Arc::new(CountingSink::default());
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let backend = Arc::new(QueueBackend::new(b"A"));
    let serial = X86SerialPortDevice::new_with_irq_and_backend(line, backend.clone());
    serial
        .inner()
        .handle_write(X86Port::new(0x3f9), X86AccessWidth::Byte, 1)
        .unwrap();

    assert!(serial.service_irq().unwrap());
    assert!(serial.service_irq().unwrap());
    assert_eq!(sink.pulses.load(Ordering::Acquire), 1);

    assert_eq!(
        serial
            .inner()
            .handle_read(X86Port::new(0x3f8), X86AccessWidth::Byte)
            .unwrap(),
        b'A' as usize
    );
    assert!(!serial.service_irq().unwrap());
    backend.push(b'B');
    assert!(serial.service_irq().unwrap());
    assert_eq!(sink.pulses.load(Ordering::Acquire), 2);
}

#[test]
fn guest_read_rearms_edge_triggered_com1_before_the_next_byte() {
    let sink = Arc::new(CountingSink::default());
    let line = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let backend = Arc::new(QueueBackend::new(b"A"));
    let serial = X86SerialPortDevice::new_with_irq_and_backend(line, backend.clone());
    serial
        .inner()
        .handle_write(X86Port::new(0x3f9), X86AccessWidth::Byte, 1)
        .unwrap();

    assert!(serial.service_irq().unwrap());
    assert!(matches!(
        serial
            .handle(&BusAccess {
                kind: BusKind::Port,
                is_read: true,
                addr: 0x3f8,
                width: AccessWidth::Byte,
                data: 0,
            })
            .unwrap(),
        BusResponse::Read { value } if value == u64::from(b'A')
    ));

    backend.push(b'B');
    assert!(serial.service_irq().unwrap());
    assert_eq!(sink.pulses.load(Ordering::Acquire), 2);
}

#[derive(Default)]
struct CountingSink {
    pulses: AtomicUsize,
}

impl WiredIrqSink for CountingSink {
    fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
        Ok(())
    }

    fn pulse(&self, _input: ControllerInputId) -> IrqResult {
        self.pulses.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct QueueBackend {
    receive_queue: Mutex<VecDeque<u8>>,
}

impl QueueBackend {
    fn new(bytes: &[u8]) -> Self {
        Self {
            receive_queue: Mutex::new(bytes.iter().copied().collect()),
        }
    }

    fn push(&self, byte: u8) {
        self.receive_queue.lock().unwrap().push_back(byte);
    }
}

impl X86SerialBackend for QueueBackend {
    fn transmit(&self, _bytes: &[u8]) {}

    fn receive(&self, bytes: &mut [u8]) -> usize {
        let mut queue = self.receive_queue.lock().unwrap();
        let count = bytes.len().min(queue.len());
        for byte in &mut bytes[..count] {
            *byte = queue.pop_front().unwrap();
        }
        count
    }
}
