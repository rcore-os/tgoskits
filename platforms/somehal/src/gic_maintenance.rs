//! Publication and validation of the discovered GIC maintenance interrupt.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use irq_framework::{HwIrq, IrqCapabilityStatus, IrqDomainId, IrqError, IrqId};

const UNINITIALIZED: u8 = 0;
const AVAILABLE: u8 = 1;
const UNAVAILABLE: u8 = 2;
const ERROR: u8 = 3;
const PUBLISHING: u8 = 4;

pub(crate) struct GicMaintenanceIrqCapability {
    state: AtomicU8,
    encoded_irq: AtomicU64,
    error: AtomicU8,
}

impl GicMaintenanceIrqCapability {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU8::new(UNINITIALIZED),
            encoded_irq: AtomicU64::new(0),
            error: AtomicU8::new(0),
        }
    }

    pub(crate) fn publish_available(&self, irq: IrqId) -> Result<(), IrqError> {
        let encoded = ((irq.domain.0 as u64) << 32) | irq.hwirq.0 as u64;
        self.begin_publication()?;
        self.encoded_irq.store(encoded, Ordering::Relaxed);
        self.state.store(AVAILABLE, Ordering::Release);
        Ok(())
    }

    pub(crate) fn publish_unavailable(&self) -> Result<(), IrqError> {
        self.publish_terminal(UNAVAILABLE)
    }

    pub(crate) fn publish_error(&self, error: IrqError) -> Result<(), IrqError> {
        self.begin_publication()?;
        self.error.store(encode_irq_error(error), Ordering::Relaxed);
        self.state.store(ERROR, Ordering::Release);
        Ok(())
    }

    pub(crate) fn status(&self) -> IrqCapabilityStatus {
        match self.state.load(Ordering::Acquire) {
            AVAILABLE => {
                let encoded = self.encoded_irq.load(Ordering::Relaxed);
                IrqCapabilityStatus::Available(IrqId::new(
                    IrqDomainId((encoded >> 32) as u16),
                    HwIrq(encoded as u32),
                ))
            }
            UNAVAILABLE => IrqCapabilityStatus::Unavailable,
            ERROR => {
                IrqCapabilityStatus::Error(decode_irq_error(self.error.load(Ordering::Relaxed)))
            }
            UNINITIALIZED | PUBLISHING => IrqCapabilityStatus::Uninitialized,
            _ => IrqCapabilityStatus::Error(IrqError::Controller),
        }
    }

    fn begin_publication(&self) -> Result<(), IrqError> {
        self.state
            .compare_exchange(
                UNINITIALIZED,
                PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| IrqError::Busy)
    }

    fn publish_terminal(&self, state: u8) -> Result<(), IrqError> {
        self.state
            .compare_exchange(UNINITIALIZED, state, Ordering::Release, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| IrqError::Busy)
    }
}

pub(crate) fn validate_irq(expected_domain: IrqDomainId, irq: IrqId) -> Result<IrqId, IrqError> {
    if irq.domain != expected_domain || !(16..32).contains(&irq.hwirq.0) {
        Err(IrqError::InvalidIrq)
    } else {
        Ok(irq)
    }
}

const fn encode_irq_error(error: IrqError) -> u8 {
    match error {
        IrqError::InvalidIrq => 1,
        IrqError::InvalidCpu => 2,
        IrqError::CpuOffline => 3,
        IrqError::Timeout => 4,
        IrqError::Busy => 5,
        IrqError::NoMemory => 6,
        IrqError::NotFound => 7,
        IrqError::InIrqContext => 8,
        IrqError::Unsupported => 9,
        IrqError::Controller => 10,
    }
}

const fn decode_irq_error(encoded: u8) -> IrqError {
    match encoded {
        1 => IrqError::InvalidIrq,
        2 => IrqError::InvalidCpu,
        3 => IrqError::CpuOffline,
        4 => IrqError::Timeout,
        5 => IrqError::Busy,
        6 => IrqError::NoMemory,
        7 => IrqError::NotFound,
        8 => IrqError::InIrqContext,
        9 => IrqError::Unsupported,
        _ => IrqError::Controller,
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn preserves_every_terminal_discovery_status() {
        let uninitialized = GicMaintenanceIrqCapability::new();
        assert_eq!(uninitialized.status(), IrqCapabilityStatus::Uninitialized);

        let available = GicMaintenanceIrqCapability::new();
        let irq = IrqId::new(IrqDomainId(23), HwIrq(25));
        available.publish_available(irq).unwrap();
        assert_eq!(available.status(), IrqCapabilityStatus::Available(irq));

        let unavailable = GicMaintenanceIrqCapability::new();
        unavailable.publish_unavailable().unwrap();
        assert_eq!(unavailable.status(), IrqCapabilityStatus::Unavailable);

        for error in [IrqError::Unsupported, IrqError::Busy] {
            let failed = GicMaintenanceIrqCapability::new();
            failed.publish_error(error).unwrap();
            assert_eq!(failed.status(), IrqCapabilityStatus::Error(error));
            assert_eq!(failed.status(), IrqCapabilityStatus::Error(error));
        }
    }

    #[test]
    fn rejects_duplicate_publication_without_changing_the_first_result() {
        let capability = GicMaintenanceIrqCapability::new();
        capability.publish_error(IrqError::Busy).unwrap();

        assert_eq!(capability.publish_unavailable(), Err(IrqError::Busy));
        assert_eq!(
            capability.status(),
            IrqCapabilityStatus::Error(IrqError::Busy)
        );
    }
}
