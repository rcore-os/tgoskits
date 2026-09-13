//! Counter configuration and active-width accesses.

use core::arch::asm;

use super::{CounterId, EventConfig, EventSupport, Pmu, PmuError, isb, registers};

impl Pmu {
    /// Configures a disabled counter without changing its value.
    pub fn configure(&mut self, id: CounterId, config: EventConfig) -> Result<(), PmuError> {
        self.validate(id)?;
        if (id == CounterId::CYCLE && config.event != 0x11)
            || (id == CounterId::INSTRUCTIONS && config.event != 0x08)
        {
            return Err(PmuError::InvalidConfiguration);
        }
        if id != CounterId::CYCLE
            && id != CounterId::INSTRUCTIONS
            && self.info.event_support(config.event) == EventSupport::Unsupported
        {
            return Err(PmuError::UnsupportedEvent);
        }
        self.disable(id)?;
        let filter = ((config.exclude_kernel as u64) << 31)
            | ((config.exclude_user as u64) << 30)
            | ((config.include_hypervisor as u64) << 27);
        if id == CounterId::CYCLE {
            write_reg!("PMCCFILTR_EL0", filter);
        } else if id == CounterId::INSTRUCTIONS {
            write_reg!("S3_3_C9_C6_0", filter | u64::from(config.event)); // PMICFILTR_EL0
        } else {
            registers::write_event(id.index(), filter | u64::from(config.event));
        }
        Ok(())
    }

    /// Enables a counter after making its configuration visible.
    pub fn enable(&mut self, id: CounterId) -> Result<(), PmuError> {
        self.validate(id)?;
        isb();
        write_reg!("PMCNTENSET_EL0", 1u64 << id.index());
        Ok(())
    }

    /// Disables a counter and synchronizes before subsequent reconfiguration.
    pub fn disable(&mut self, id: CounterId) -> Result<(), PmuError> {
        self.validate(id)?;
        write_reg!("PMCNTENCLR_EL0", 1u64 << id.index());
        isb();
        Ok(())
    }

    /// Reads a counter with its active overflow width.
    pub fn read(&self, id: CounterId) -> Result<u64, PmuError> {
        let mask = self.mask(id)?;
        let value = if id == CounterId::CYCLE {
            read_reg!("PMCCNTR_EL0")
        } else if id == CounterId::INSTRUCTIONS {
            read_reg!("S3_3_C9_C4_0") // PMICNTR_EL0
        } else {
            registers::read_counter(id.index())
        };
        Ok(value & mask)
    }

    /// Writes the low active-width bits of a counter.
    pub fn write(&mut self, id: CounterId, value: u64) -> Result<(), PmuError> {
        let value = value & self.mask(id)?;
        if id == CounterId::CYCLE {
            write_reg!("PMCCNTR_EL0", value);
        } else if id == CounterId::INSTRUCTIONS {
            write_reg!("S3_3_C9_C4_0", value); // PMICNTR_EL0
        } else {
            registers::write_counter(id.index(), value);
        }
        Ok(())
    }

    /// Preloads a nonzero sampling period using the active overflow width.
    pub fn preload(&mut self, id: CounterId, period: u64) -> Result<(), PmuError> {
        if period == 0 || period > self.mask(id)? {
            return Err(PmuError::InvalidPeriod);
        }
        self.write(id, 0u64.wrapping_sub(period))
    }
}
