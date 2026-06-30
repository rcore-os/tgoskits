use crate::irq::{CPU_LOCAL_IRQ_DOMAIN, HwIrq, IrqError, IrqId, X86_LAPIC_DOMAIN};

pub(super) const APIC_TIMER_VECTOR: usize = 0x20;
pub(super) const APIC_IPI_VECTOR: usize = 0xf3;
pub(super) const SPURIOUS_VECTOR: usize = 0xff;

pub(super) fn lapic_timer_irq_id() -> IrqId {
    IrqId::new(X86_LAPIC_DOMAIN, HwIrq(0))
}

pub(super) fn lapic_ipi_irq_id() -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(APIC_IPI_VECTOR as u32))
}

#[cfg(test)]
pub(super) fn ioapic_gsi_irq_id(gsi: u32) -> IrqId {
    IrqId::new(crate::irq::IrqDomainId(7), HwIrq(gsi))
}

pub(super) fn local_vector_irq_id(raw: usize) -> Option<IrqId> {
    if raw == APIC_TIMER_VECTOR {
        return Some(lapic_timer_irq_id());
    }

    if raw == APIC_IPI_VECTOR {
        return Some(lapic_ipi_irq_id());
    }

    None
}

pub(super) fn validate_external_vector(vector: usize) -> Result<u8, IrqError> {
    let vector_u8 = u8::try_from(vector).map_err(|_| IrqError::InvalidIrq)?;
    if vector < 0x20
        || matches!(
            vector,
            APIC_TIMER_VECTOR | APIC_IPI_VECTOR | SPURIOUS_VECTOR
        )
    {
        return Err(IrqError::Busy);
    }
    Ok(vector_u8)
}
