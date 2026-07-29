use super::*;

impl UartPort for Pl011 {
    fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        // Runtime startup inherits a possibly active boot console. Do not wait
        // for BUSY or flush its TX FIFO while the CPU-affine worker holds the
        // IRQ exclusion boundary. Linux likewise programs normal PL011
        // startup/termios state without using the polling-console BUSY drain.
        self.mask_all();
        self.registers().uarticr.set(ALL_IRQ_BITS);
        self.set_config(config)?;
        self.registers().uartlcr_h.modify(UARTLCR_H::FEN::SET);
        self.registers()
            .uartcr
            .modify(UARTCR::UARTEN::SET + UARTCR::TXE::SET + UARTCR::RXE::SET);
        Ok(())
    }

    fn shutdown(&mut self) {
        self.registers().uartimsc.set(0);
        self.registers().uartcr.modify(UARTCR::UARTEN::CLEAR);
    }

    fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        // Keep runtime configuration a bounded register transaction. Waiting
        // for the transmitter belongs only to the polling early-console path;
        // doing it here would extend the runtime's IRQ-off register section by
        // up to BUSY_POLL_BUDGET iterations.
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
        Ok(())
    }

    fn read_rx(&mut self) -> Option<RxSample> {
        Pl011::read_rx(self)
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
