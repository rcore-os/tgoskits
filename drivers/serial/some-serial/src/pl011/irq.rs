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
    fn mask(&mut self, sources: SerialEventSet) {
        let enabled = self.registers().uartimsc.get();
        self.registers()
            .uartimsc
            .set(enabled & !imsc_for_events(sources));
    }

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
        let tx_rearm = events & SerialEventSet::TX_SPACE;
        if !tx_rearm.is_empty() && !events.contains(SerialEventSet::FAULT) {
            self.mask(tx_rearm);
        }
        // Retire RX/RT by reading UARTDR, as in Linux pl011_int. Explicitly
        // clearing them can erase a new arrival after the final RXFE check;
        // QEMU will not reassert RX until the FIFO crosses its threshold again.
        // TX is already masked, and sampled error/modem latches are cleared
        // before draining so later events remain pending for the next IRQ.
        self.registers()
            .uarticr
            .set(active & !(UARTIS::RX::SET.value | UARTIS::RT::SET.value));
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

        let mut rearm = tx_rearm;
        if rx.len() == IRQ_RX_BATCH_CAPACITY || rx_errors.contains(RxErrorFlags::OVERRUN) {
            rearm |= SerialEventSet::RX;
        }
        if events.contains(SerialEventSet::FAULT) {
            self.registers().uartimsc.set(0);
        } else if rearm.intersects(SerialEventSet::RX) {
            self.mask(SerialEventSet::RX);
        }

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
