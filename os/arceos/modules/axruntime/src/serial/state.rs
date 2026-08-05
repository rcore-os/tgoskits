use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rdif_serial::{RxErrorFlags, SerialEventSet, SerialIrqEvent};

const ERROR_SHIFT: u32 = 8;
const REARM_SHIFT: u32 = 16;

/// One atomic latch keeps every field of an IRQ observation in the same batch.
pub(super) struct SerialIrqLatch {
    packed: AtomicU32,
}

impl SerialIrqLatch {
    pub(super) const fn new() -> Self {
        Self {
            packed: AtomicU32::new(0),
        }
    }

    pub(super) fn publish(&self, event: SerialIrqEvent) {
        self.packed.fetch_or(pack(event), Ordering::Release);
    }

    pub(super) fn take(&self) -> Option<SerialIrqEvent> {
        unpack(self.packed.swap(0, Ordering::AcqRel))
    }

    pub(super) fn has_pending(&self) -> bool {
        self.packed.load(Ordering::Acquire) != 0
    }
}

const fn pack(event: SerialIrqEvent) -> u32 {
    event.events.bits()
        | (event.rx_errors.bits() << ERROR_SHIFT)
        | (event.rearm.bits() << REARM_SHIFT)
}

fn unpack(packed: u32) -> Option<SerialIrqEvent> {
    if packed == 0 {
        return None;
    }
    Some(SerialIrqEvent {
        events: SerialEventSet::from_bits_retain(packed & SerialEventSet::all().bits()),
        rx_errors: RxErrorFlags::from_bits_retain(
            (packed >> ERROR_SHIFT) & RxErrorFlags::all().bits(),
        ),
        rearm: SerialEventSet::from_bits_retain(
            (packed >> REARM_SHIFT) & SerialEventSet::all().bits(),
        ),
    })
}

pub(super) struct SerialStatsAtomic {
    handled_irq: AtomicU64,
    spurious_irq: AtomicU64,
    fault_irq: AtomicU64,
    rx_bytes: AtomicU64,
    rx_errors: AtomicU64,
    rx_dropped: AtomicU64,
    tx_bytes: AtomicU64,
    log_dropped: AtomicU64,
}

impl SerialStatsAtomic {
    pub(super) const fn new() -> Self {
        Self {
            handled_irq: AtomicU64::new(0),
            spurious_irq: AtomicU64::new(0),
            fault_irq: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            rx_errors: AtomicU64::new(0),
            rx_dropped: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            log_dropped: AtomicU64::new(0),
        }
    }

    pub(super) fn handled_irq(&self, event: SerialIrqEvent) {
        self.handled_irq.fetch_add(1, Ordering::Relaxed);
        if event.events.contains(SerialEventSet::FAULT) {
            self.fault_irq.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn spurious_irq(&self) {
        self.spurious_irq.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn add_rx_bytes(&self, count: usize) {
        self.rx_bytes.fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn add_rx_errors(&self, count: u32) {
        self.rx_errors
            .fetch_add(u64::from(count), Ordering::Relaxed);
    }

    pub(super) fn add_rx_dropped(&self, count: usize) {
        self.rx_dropped.fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn add_tx_bytes(&self, count: usize) {
        self.tx_bytes.fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn add_log_dropped(&self, count: usize) {
        self.log_dropped.fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> SerialStats {
        SerialStats {
            handled_irq: self.handled_irq.load(Ordering::Relaxed),
            spurious_irq: self.spurious_irq.load(Ordering::Relaxed),
            fault_irq: self.fault_irq.load(Ordering::Relaxed),
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed),
            rx_errors: self.rx_errors.load(Ordering::Relaxed),
            rx_dropped: self.rx_dropped.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            log_dropped: self.log_dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialStats {
    pub handled_irq: u64,
    pub spurious_irq: u64,
    pub fault_irq: u64,
    pub rx_bytes: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub log_dropped: u64,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use std::{sync::Barrier, thread};

    use super::*;

    #[test]
    fn publish_coalesces_complete_events_until_worker_take() {
        let latch = SerialIrqLatch::new();
        latch.publish(SerialIrqEvent {
            events: SerialEventSet::RX_DATA,
            rx_errors: RxErrorFlags::PARITY,
            rearm: SerialEventSet::RX,
        });
        latch.publish(SerialIrqEvent {
            events: SerialEventSet::TX_SPACE,
            rx_errors: RxErrorFlags::empty(),
            rearm: SerialEventSet::TX_SPACE,
        });

        let event = latch.take().unwrap();
        assert!(event.events.contains(SerialEventSet::RX_DATA));
        assert!(event.events.contains(SerialEventSet::TX_SPACE));
        assert_eq!(event.rx_errors, RxErrorFlags::PARITY);
        assert_eq!(event.rearm, SerialEventSet::RX | SerialEventSet::TX_SPACE);
        assert!(!latch.has_pending());
    }

    #[test]
    fn irq_event_fields_cannot_be_split_across_worker_takes() {
        let latch = Arc::new(SerialIrqLatch::new());
        latch.publish(SerialIrqEvent {
            events: SerialEventSet::RX_DATA,
            ..SerialIrqEvent::default()
        });
        let start = Arc::new(Barrier::new(2));
        let publisher = {
            let latch = latch.clone();
            let start = start.clone();
            thread::spawn(move || {
                start.wait();
                latch.publish(SerialIrqEvent {
                    events: SerialEventSet::TX_SPACE,
                    rx_errors: RxErrorFlags::PARITY,
                    rearm: SerialEventSet::TX_SPACE,
                });
            })
        };

        start.wait();
        let first = latch.take();
        publisher.join().unwrap();
        let second = latch.take();

        let mut observed = [first, second].into_iter().flatten();
        let mut events = SerialEventSet::empty();
        let mut errors = RxErrorFlags::empty();
        let mut rearm = SerialEventSet::empty();
        for event in observed.by_ref() {
            if event.events.contains(SerialEventSet::TX_SPACE) {
                assert_eq!(event.rx_errors, RxErrorFlags::PARITY);
                assert_eq!(event.rearm, SerialEventSet::TX_SPACE);
            }
            events |= event.events;
            errors |= event.rx_errors;
            rearm |= event.rearm;
        }
        assert_eq!(events, SerialEventSet::RX_DATA | SerialEventSet::TX_SPACE);
        assert_eq!(errors, RxErrorFlags::PARITY);
        assert_eq!(rearm, SerialEventSet::TX_SPACE);
    }
}
