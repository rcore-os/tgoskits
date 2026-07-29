use super::*;

pub(super) fn read_rx_sample(
    registers: &Pl011Registers,
    saved_status: &mut Pl011RxStatus,
) -> Option<RxSample> {
    if registers.uartfr.is_set(UARTFR::RXFE) {
        *saved_status |= Pl011RxStatus::from_rsr(registers.uartrsr_ecr.extract());
        return saved_status.take_status_sample();
    }

    let dr = registers.uartdr.extract();
    let data = dr.read(UARTDR::DATA) as u8;
    let status = Pl011RxStatus::from_data(dr);
    if !status.is_empty() {
        saved_status.remove(status);
    }

    Some(RxSample {
        byte: Some(data),
        flag: status.flag(),
        overrun: status.contains(Pl011RxStatus::OVERRUN),
    })
}

pub(super) fn rx_errors_from_sample(sample: RxSample) -> RxErrorFlags {
    let mut errors = match sample.flag {
        RxFlag::Normal => RxErrorFlags::empty(),
        RxFlag::Break => RxErrorFlags::BREAK,
        RxFlag::Parity => RxErrorFlags::PARITY,
        RxFlag::Framing => RxErrorFlags::FRAMING,
    };
    if sample.overrun {
        errors |= RxErrorFlags::OVERRUN;
    }
    errors
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(super) struct Pl011RxStatus: u32 {
        const FRAMING = 1 << 0;
        const PARITY  = 1 << 1;
        const BREAK   = 1 << 2;
        const OVERRUN = 1 << 3;
    }
}

impl Pl011RxStatus {
    pub(super) fn to_irq_errors(self) -> RxErrorFlags {
        let mut errors = RxErrorFlags::empty();
        if self.contains(Self::BREAK) {
            errors |= RxErrorFlags::BREAK;
        }
        if self.contains(Self::PARITY) {
            errors |= RxErrorFlags::PARITY;
        }
        if self.contains(Self::FRAMING) {
            errors |= RxErrorFlags::FRAMING;
        }
        if self.contains(Self::OVERRUN) {
            errors |= RxErrorFlags::OVERRUN;
        }
        errors
    }

    fn from_data(dr: LocalRegisterCopy<u32, UARTDR::Register>) -> Self {
        let mut status = Self::empty();
        if dr.is_set(UARTDR::FE) {
            status |= Self::FRAMING;
        }
        if dr.is_set(UARTDR::PE) {
            status |= Self::PARITY;
        }
        if dr.is_set(UARTDR::BE) {
            status |= Self::BREAK;
        }
        if dr.is_set(UARTDR::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    pub(super) fn from_irq_status(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> Self {
        let mut status = Self::empty();
        if mis.is_set(UARTIS::FE) {
            status |= Self::FRAMING;
        }
        if mis.is_set(UARTIS::PE) {
            status |= Self::PARITY;
        }
        if mis.is_set(UARTIS::BE) {
            status |= Self::BREAK;
        }
        if mis.is_set(UARTIS::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    pub(super) fn from_rsr(rsr: LocalRegisterCopy<u32, UARTRSR_ECR::Register>) -> Self {
        let mut status = Self::empty();
        if rsr.is_set(UARTRSR_ECR::FE) {
            status |= Self::FRAMING;
        }
        if rsr.is_set(UARTRSR_ECR::PE) {
            status |= Self::PARITY;
        }
        if rsr.is_set(UARTRSR_ECR::BE) {
            status |= Self::BREAK;
        }
        if rsr.is_set(UARTRSR_ECR::OE) {
            status |= Self::OVERRUN;
        }
        status
    }

    fn flag(self) -> RxFlag {
        if self.contains(Self::BREAK) {
            RxFlag::Break
        } else if self.contains(Self::PARITY) {
            RxFlag::Parity
        } else if self.contains(Self::FRAMING) {
            RxFlag::Framing
        } else {
            RxFlag::Normal
        }
    }

    fn take_status_sample(&mut self) -> Option<RxSample> {
        if self.is_empty() {
            return None;
        }

        let status = *self;
        *self = Self::empty();
        Some(RxSample {
            byte: None,
            flag: status.flag(),
            overrun: status.contains(Self::OVERRUN),
        })
    }
}
