use super::*;

#[derive(Clone, Copy)]
struct Pl011ConfigSnapshot {
    ilpr: u32,
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    dmacr: u32,
}

impl Pl011ConfigSnapshot {
    fn capture(registers: &Pl011Registers) -> Self {
        Self {
            ilpr: registers.uartilpr.get(),
            ibrd: registers.uartibrd.get(),
            fbrd: registers.uartfbrd.get(),
            lcr_h: registers.uartlcr_h.get(),
            cr: registers.uartcr.get(),
            ifls: registers.uartifls.get(),
            imsc: registers.uartimsc.get(),
            dmacr: registers.uartdmacr.get(),
        }
    }

    fn restore(self, registers: &Pl011Registers) {
        registers.uartilpr.set(self.ilpr);
        registers.uartibrd.set(self.ibrd);
        registers.uartfbrd.set(self.fbrd);
        registers.uartlcr_h.set(self.lcr_h);
        registers.uartifls.set(self.ifls);
        registers.uartimsc.set(self.imsc);
        registers.uartdmacr.set(self.dmacr);
        // Restore CR last so the original enable state is not published until
        // every dependent configuration register is back in place.
        registers.uartcr.set(self.cr);
    }
}

impl UartPort for Pl011 {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        let snapshot = Pl011ConfigSnapshot::capture(self.registers());
        if let Err(error) = self.open().and_then(|()| self.set_config(config)) {
            snapshot.restore(self.registers());
            return Err(error);
        }
        self.mask_all();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.registers().uartimsc.set(0);
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        let snapshot = Pl011ConfigSnapshot::capture(self.registers());
        let result = (|| {
            self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);
            self.wait_until_not_busy()?;

            self.registers().uartlcr_h.modify(UARTLCR_H::FEN::CLEAR);
            if let Some(baudrate) = config.baudrate {
                self.set_baudrate_internal(baudrate)?;
            }
            if let Some(data_bits) = config.data_bits {
                self.set_data_bits_internal(data_bits)?;
            }
            if let Some(stop_bits) = config.stop_bits {
                self.set_stop_bits_internal(stop_bits)?;
            }
            if let Some(parity) = config.parity {
                self.set_parity_internal(parity)?;
            }
            self.registers().uartlcr_h.modify(UARTLCR_H::FEN::SET);
            self.registers().uartcr.set(snapshot.cr);
            Ok(())
        })();

        if result.is_err() {
            snapshot.restore(self.registers());
        }
        result
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        Pl011::read_rx(self)
    }

    fn discard_rx(&mut self) {
        // PL011 has no independent RX FIFO reset bit. The hardware FIFO holds
        // at most 32 bytes, so consume one fixed-capacity snapshot without
        // perturbing the TX path or disabling the whole UART.
        for _ in 0..32 {
            if self.registers().uartfr.is_set(UARTFR::RXFE) {
                break;
            }
            let _ = self.registers().uartdr.get();
        }
        self.registers().uartrsr_ecr.set(0);
        self.saved_rx_status = Pl011RxStatus::empty();
        self.registers()
            .uarticr
            .set(imsc_for_events(SerialEventSet::RX));
    }

    fn discard_tx(&mut self) -> bool {
        // Clearing FEN or UARTEN would also disturb RX. PL011 cannot flush the
        // TX FIFO independently, so report the unsupported operation without
        // changing shared UART state.
        false
    }

    fn write_tx(&mut self, bytes: &[u8]) -> usize {
        let mut written = 0;
        for &byte in bytes {
            if self.registers().uartfr.is_set(UARTFR::TXFF) {
                break;
            }
            self.registers().uartdr.set(byte as u32);
            written += 1;
        }
        written
    }

    fn tx_idle(&mut self) -> bool {
        let fr = self.registers().uartfr.extract();
        !fr.is_set(UARTFR::BUSY) && !fr.is_set(UARTFR::TXFF)
    }

    fn mask(&mut self, sources: SerialEventSet) {
        let enabled = self.registers().uartimsc.get();
        self.registers()
            .uartimsc
            .set(enabled & !imsc_for_events(sources));
    }

    fn mask_all(&mut self) {
        self.registers().uartimsc.set(0);
    }

    fn rearm(&mut self, sources: SerialEventSet) -> SerialEventSet {
        let enabled = self.registers().uartimsc.get() | imsc_for_events(sources);
        self.registers().uartimsc.set(enabled);

        let fr = self.registers().uartfr.extract();
        let rsr = self.registers().uartrsr_ecr.extract();
        let mut ready = SerialEventSet::empty();
        if sources.intersects(SerialEventSet::RX) && !fr.is_set(UARTFR::RXFE) {
            ready |= SerialEventSet::RX_DATA;
        }
        if sources.contains(SerialEventSet::RX_STATUS) && !Pl011RxStatus::from_rsr(rsr).is_empty() {
            ready |= SerialEventSet::RX_STATUS;
        }
        if sources.contains(SerialEventSet::TX_SPACE) && !fr.is_set(UARTFR::TXFF) {
            ready |= SerialEventSet::TX_SPACE;
        }
        if !ready.is_empty() {
            self.registers()
                .uartimsc
                .set(enabled & !imsc_for_events(ready));
        }
        ready
    }
}

impl SplitUart for Pl011 {
    type Control = Self;
    type Irq = Pl011Irq;
    type EmergencyTx = Pl011EmergencyTx;

    fn runtime_info(&self) -> UartInfo {
        UartInfo {
            name: "PL011 UART",
            register_base: self.base.0.as_ptr() as usize,
            initial_baudrate: self.current_baudrate(),
        }
    }

    fn split(self) -> SerialParts<Self::Control, Self::Irq, Self::EmergencyTx> {
        let irq = Pl011Irq {
            base: self.base,
            saved_rx_status: Pl011RxStatus::empty(),
        };
        let emergency_tx = Pl011EmergencyTx { base: self.base };
        SerialParts::new(self, irq, emergency_tx)
    }
}

impl PollingUart for Pl011 {
    fn poll_status(&mut self) -> SerialEvent {
        Pl011::poll_status(self)
    }

    fn write_byte(&mut self, byte: u8) {
        Pl011::write_byte(self, byte);
    }

    fn read_byte(&mut self, status: SerialEvent) -> Option<Result<u8, TransferError>> {
        Pl011::read_byte(self, status)
    }
}
