//! Pure validation rules for the discovered GIC maintenance interrupt.

use irq_framework::{IrqDomainId, IrqError, IrqId};

pub(crate) fn validate_irq(expected_domain: IrqDomainId, irq: IrqId) -> Result<IrqId, IrqError> {
    if irq.domain != expected_domain || !(16..32).contains(&irq.hwirq.0) {
        Err(IrqError::InvalidIrq)
    } else {
        Ok(irq)
    }
}

#[cfg(test)]
mod tests {
    use irq_framework::HwIrq;

    use super::*;

    #[test]
    fn accepts_only_the_exact_gic_domain_and_ppi_range() {
        let domain = IrqDomainId(23);
        let first_ppi = IrqId::new(domain, HwIrq(16));
        let last_ppi = IrqId::new(domain, HwIrq(31));
        assert_eq!(validate_irq(domain, first_ppi), Ok(first_ppi));
        assert_eq!(validate_irq(domain, last_ppi), Ok(last_ppi));
        assert_eq!(
            validate_irq(IrqDomainId(24), first_ppi),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            validate_irq(domain, IrqId::new(domain, HwIrq(15))),
            Err(IrqError::InvalidIrq)
        );
        assert_eq!(
            validate_irq(domain, IrqId::new(domain, HwIrq(32))),
            Err(IrqError::InvalidIrq)
        );
    }
}
