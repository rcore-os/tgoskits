use super::*;

pub(super) fn events_from_mis(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> SerialEventSet {
    let mut events = SerialEventSet::empty();
    if mis.is_set(UARTIS::RX) {
        events |= SerialEventSet::RX_DATA;
    }
    if mis.is_set(UARTIS::RT) {
        events |= SerialEventSet::RX_TIMEOUT;
    }
    if mis.is_set(UARTIS::FE)
        || mis.is_set(UARTIS::PE)
        || mis.is_set(UARTIS::BE)
        || mis.is_set(UARTIS::OE)
    {
        events |= SerialEventSet::RX_STATUS;
    }
    if mis.is_set(UARTIS::TX) {
        events |= SerialEventSet::TX_SPACE;
    }
    if mis.is_set(UARTIS::CTSM)
        || mis.is_set(UARTIS::DSRM)
        || mis.is_set(UARTIS::DCDM)
        || mis.is_set(UARTIS::RIM)
    {
        events |= SerialEventSet::MODEM_STATUS;
    }
    events
}

pub(super) fn rx_errors_from_mis(mis: LocalRegisterCopy<u32, UARTIS::Register>) -> RxErrorFlags {
    Pl011RxStatus::from_irq_status(mis).to_irq_errors()
}

pub(super) fn imsc_for_events(events: SerialEventSet) -> u32 {
    let mut imsc = 0;
    if events.intersects(SerialEventSet::RX) {
        imsc |= UARTIS::RX::SET.value
            | UARTIS::RT::SET.value
            | UARTIS::FE::SET.value
            | UARTIS::PE::SET.value
            | UARTIS::BE::SET.value
            | UARTIS::OE::SET.value;
    }
    if events.contains(SerialEventSet::TX_SPACE) {
        imsc |= UARTIS::TX::SET.value;
    }
    if events.contains(SerialEventSet::MODEM_STATUS) {
        imsc |= UARTIS::RIM::SET.value
            | UARTIS::CTSM::SET.value
            | UARTIS::DCDM::SET.value
            | UARTIS::DSRM::SET.value;
    }
    imsc
}
