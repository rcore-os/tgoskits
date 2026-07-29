use super::*;

/// IRQ-only endpoint for a PL011 UART.
pub struct Pl011Irq {
    pub(super) base: Reg,
    pub(super) saved_rx_status: Pl011RxStatus,
}

impl Pl011Irq {
    fn registers(&self) -> &Pl011Registers {
        // SAFETY: `base` points at the mapped PL011 register block. The IRQ
        // endpoint intentionally exposes no FIFO data methods.
        unsafe { &*self.base.0.as_ptr() }
    }
}

impl UartIrq for Pl011Irq {
    fn handle(&mut self) -> Option<SerialIrqReport> {
        let mis = self.registers().uartmis.extract();
        let active = mis.get();
        if active == 0 {
            return None;
        }

        let mut events = events_from_mis(mis);
        let mut rx = IrqRxBatch::new();
        if active & !ALL_IRQ_BITS != 0 {
            events |= SerialEventSet::FAULT;
        }
        let mut rx_errors = rx_errors_from_mis(mis);
        if events.intersects(SerialEventSet::RX) {
            let base = self.base;
            // SAFETY: `base` is the mapped PL011 register block shared with
            // the task endpoint under the runtime's same-CPU exclusion rule.
            let registers = unsafe { &*base.0.as_ptr() };
            for _ in 0..IRQ_RX_BATCH_CAPACITY {
                let Some(sample) = read_rx_sample(registers, &mut self.saved_rx_status) else {
                    break;
                };
                rx_errors |= rx_errors_from_sample(sample);
                rx.try_push(sample)
                    .expect("the fixed PL011 IRQ loop cannot overflow its RX batch");
            }
        }

        let rearm = events & SerialEventSet::TX_SPACE;
        if events.contains(SerialEventSet::FAULT) {
            self.registers().uartimsc.set(0);
        } else if !rearm.is_empty() {
            let enabled = self.registers().uartimsc.get();
            self.registers()
                .uartimsc
                .set(enabled & !imsc_for_events(rearm));
        }
        self.registers().uarticr.set(active);

        Some(SerialIrqReport::new(
            SerialIrqEvent {
                events,
                rx_errors,
                rearm,
            },
            rx,
        ))
    }
}
