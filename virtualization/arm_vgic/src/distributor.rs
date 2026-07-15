//! Shared GICv3 Distributor state.

use alloc::vec::Vec;

use axvm_types::AccessWidth;

use crate::{
    GicAffinity, GicV3Config, IntId, InterruptRecord, InterruptState, Priority, RegisterRegion,
    SpiId, TriggerMode, VgicError, VgicResult,
    register::{
        GICD_CTLR, GICD_ICACTIVER, GICD_ICENABLER, GICD_ICFGR, GICD_ICPENDR, GICD_IGROUPR,
        GICD_IIDR, GICD_IPRIORITYR, GICD_IROUTER, GICD_ISACTIVER, GICD_ISENABLER, GICD_ISPENDR,
        GICD_TYPER, GicComponent, component_id,
    },
};

pub(crate) struct DistributorState {
    enabled: bool,
    interrupts: Vec<InterruptRecord>,
}

impl DistributorState {
    pub(crate) fn new(spi_count: usize) -> VgicResult<Self> {
        let mut interrupts = Vec::with_capacity(spi_count);
        for index in 0..spi_count {
            let raw = 32 + index as u32;
            interrupts.push(InterruptRecord::new(
                IntId::Spi(SpiId::new(raw)?),
                TriggerMode::Level,
            ));
        }
        Ok(Self {
            enabled: false,
            interrupts,
        })
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn interrupt(&self, spi: SpiId) -> VgicResult<&InterruptRecord> {
        self.interrupts
            .get((spi.raw() - 32) as usize)
            .ok_or(VgicError::InvalidIntId { raw: spi.raw() })
    }

    pub(crate) fn interrupt_mut(&mut self, spi: SpiId) -> VgicResult<&mut InterruptRecord> {
        self.interrupts
            .get_mut((spi.raw() - 32) as usize)
            .ok_or(VgicError::InvalidIntId { raw: spi.raw() })
    }

    pub(crate) fn set_trigger(&mut self, spi: SpiId, trigger: TriggerMode) -> VgicResult {
        self.interrupt_mut(spi)?.set_trigger(trigger);
        Ok(())
    }

    pub(crate) fn set_route(&mut self, spi: SpiId, route: GicAffinity) -> VgicResult {
        self.interrupt_mut(spi)?.set_route(route);
        Ok(())
    }

    pub(crate) fn set_level(&mut self, spi: SpiId, asserted: bool) -> VgicResult {
        self.interrupt_mut(spi)?.set_level(asserted);
        Ok(())
    }

    pub(crate) fn pulse(&mut self, spi: SpiId) -> VgicResult {
        self.interrupt_mut(spi)?.pulse();
        Ok(())
    }

    pub(crate) fn state(&self, spi: SpiId) -> VgicResult<InterruptState> {
        Ok(self.interrupt(spi)?.state())
    }

    pub(crate) fn read(
        &self,
        offset: u64,
        width: AccessWidth,
        config: &GicV3Config,
    ) -> VgicResult<u64> {
        validate_access(offset, width, config, "read")?;
        if let Some(value) = component_id(offset, GicComponent::Distributor) {
            require_width(offset, width, AccessWidth::Dword, "read")?;
            return Ok(value);
        }
        match offset {
            GICD_CTLR => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok((self.enabled as u64) << 1 | 1 << 4)
            }
            GICD_TYPER => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                let interrupt_lines = config.spi_limit().div_ceil(32);
                let lpi_support = u64::from(config.its().is_some()) << 17;
                let largest_intid = config
                    .its()
                    .map_or(config.spi_limit() - 1, |_| config.lpi_limit());
                let interrupt_id_bits = (u32::BITS - largest_intid.leading_zeros()).max(16);
                Ok((interrupt_lines.saturating_sub(1) as u64)
                    | lpi_support
                    | (u64::from(interrupt_id_bits - 1) << 19)
                    | (1 << 24)
                    | (1 << 26))
            }
            GICD_IIDR => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(0x43b)
            }
            _ if word_index(offset, GICD_IGROUPR, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(u32::MAX as u64)
            }
            _ if word_index(offset, GICD_ISENABLER, 32).is_some()
                || word_index(offset, GICD_ICENABLER, 32).is_some() =>
            {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                self.read_flags(offset, GICD_ISENABLER, |interrupt| interrupt.enabled())
            }
            _ if word_index(offset, GICD_ISPENDR, 32).is_some()
                || word_index(offset, GICD_ICPENDR, 32).is_some() =>
            {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                self.read_flags(offset, GICD_ISPENDR, |interrupt| interrupt.pending())
            }
            _ if word_index(offset, GICD_ISACTIVER, 32).is_some()
                || word_index(offset, GICD_ICACTIVER, 32).is_some() =>
            {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                self.read_flags(offset, GICD_ISACTIVER, |interrupt| interrupt.active())
            }
            _ if (GICD_IPRIORITYR..GICD_IPRIORITYR + 1020).contains(&offset) => {
                self.read_priorities(offset, width)
            }
            _ if word_index(offset, GICD_ICFGR, 64).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                self.read_configuration(offset)
            }
            _ if (GICD_IROUTER..GICD_IROUTER + 1020 * 8).contains(&offset) => {
                require_width(offset, width, AccessWidth::Qword, "read")?;
                let raw = ((offset - GICD_IROUTER) / 8) as u32;
                if raw < 32 || raw >= config.spi_limit() {
                    Ok(0)
                } else {
                    Ok(self
                        .interrupt(SpiId::new(raw)?)?
                        .route()
                        .map_or(0, GicAffinity::mpidr))
                }
            }
            _ => Ok(0),
        }
    }

    pub(crate) fn write(
        &mut self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        config: &GicV3Config,
    ) -> VgicResult<Vec<SpiId>> {
        validate_access(offset, width, config, "write")?;
        if component_id(offset, GicComponent::Distributor).is_some() {
            require_width(offset, width, AccessWidth::Dword, "write")?;
            return Ok(Vec::new());
        }
        let mut candidates = Vec::new();
        match offset {
            GICD_CTLR => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.enabled = value & (1 << 1) != 0;
                if self.enabled {
                    candidates.extend(
                        self.interrupts
                            .iter()
                            .filter(|interrupt| interrupt.deliverable())
                            .filter_map(|interrupt| match interrupt.intid() {
                                IntId::Spi(spi) => Some(spi),
                                _ => None,
                            }),
                    );
                }
            }
            _ if word_index(offset, GICD_ISENABLER, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_flags(offset, GICD_ISENABLER, value, |interrupt| {
                    interrupt.set_enabled(true)
                })?;
            }
            _ if word_index(offset, GICD_ICENABLER, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_flags(offset, GICD_ICENABLER, value, |interrupt| {
                    interrupt.set_enabled(false)
                })?;
            }
            _ if word_index(offset, GICD_ISPENDR, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_flags(offset, GICD_ISPENDR, value, |interrupt| {
                    interrupt.set_pending(true)
                })?;
            }
            _ if word_index(offset, GICD_ICPENDR, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_flags(offset, GICD_ICPENDR, value, |interrupt| {
                    interrupt.set_pending(false)
                })?;
            }
            _ if word_index(offset, GICD_ISACTIVER, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_flags(offset, GICD_ISACTIVER, value, |interrupt| {
                    interrupt.set_active(true)
                })?;
            }
            _ if word_index(offset, GICD_ICACTIVER, 32).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_flags(offset, GICD_ICACTIVER, value, |interrupt| {
                    interrupt.complete()
                })?;
            }
            _ if (GICD_IPRIORITYR..GICD_IPRIORITYR + 1020).contains(&offset) => {
                self.write_priorities(offset, width, value)?;
            }
            _ if word_index(offset, GICD_ICFGR, 64).is_some() => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_configuration(offset, value)?;
            }
            _ if (GICD_IROUTER..GICD_IROUTER + 1020 * 8).contains(&offset) => {
                require_width(offset, width, AccessWidth::Qword, "write")?;
                let raw = ((offset - GICD_IROUTER) / 8) as u32;
                if raw >= 32 && raw < config.spi_limit() {
                    let spi = SpiId::new(raw)?;
                    self.set_route(spi, GicAffinity::from_mpidr(value))?;
                    if self.interrupt(spi)?.deliverable() {
                        candidates.push(spi);
                    }
                }
            }
            _ => {}
        }
        Ok(candidates)
    }

    fn read_flags(
        &self,
        offset: u64,
        canonical_base: u64,
        predicate: impl Fn(&InterruptRecord) -> bool,
    ) -> VgicResult<u64> {
        let index = ((offset - canonical_base) % 0x80) / 4;
        let mut value = 0u64;
        for bit in 0..32u32 {
            let raw = index as u32 * 32 + bit;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && self.interrupt(spi).is_ok_and(&predicate)
            {
                value |= 1 << bit;
            }
        }
        Ok(value)
    }

    fn write_flags(
        &mut self,
        offset: u64,
        canonical_base: u64,
        value: u64,
        mut update: impl FnMut(&mut InterruptRecord),
    ) -> VgicResult<Vec<SpiId>> {
        let index = ((offset - canonical_base) % 0x80) / 4;
        let mut candidates = Vec::new();
        for bit in 0..32u32 {
            if value & (1 << bit) == 0 {
                continue;
            }
            let raw = index as u32 * 32 + bit;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && let Ok(interrupt) = self.interrupt_mut(spi)
            {
                update(interrupt);
                if interrupt.deliverable() {
                    candidates.push(spi);
                }
            }
        }
        Ok(candidates)
    }

    fn read_priorities(&self, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        let mut value = 0;
        for byte in 0..width.size() {
            let raw = (offset - GICD_IPRIORITYR) as u32 + byte as u32;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && let Ok(interrupt) = self.interrupt(spi)
            {
                value |= (interrupt.priority().raw() as u64) << (byte * 8);
            }
        }
        Ok(value)
    }

    fn write_priorities(&mut self, offset: u64, width: AccessWidth, value: u64) -> VgicResult {
        for byte in 0..width.size() {
            let raw = (offset - GICD_IPRIORITYR) as u32 + byte as u32;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && let Ok(interrupt) = self.interrupt_mut(spi)
            {
                interrupt.set_priority(Priority::new((value >> (byte * 8)) as u8));
            }
        }
        Ok(())
    }

    fn read_configuration(&self, offset: u64) -> VgicResult<u64> {
        let index = (offset - GICD_ICFGR) / 4;
        let mut value = 0;
        for entry in 0..16u32 {
            let raw = index as u32 * 16 + entry;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && self
                    .interrupt(spi)
                    .is_ok_and(|interrupt| interrupt.trigger() == TriggerMode::Edge)
            {
                value |= 0b10 << (entry * 2);
            }
        }
        Ok(value)
    }

    fn write_configuration(&mut self, offset: u64, value: u64) -> VgicResult {
        let index = (offset - GICD_ICFGR) / 4;
        for entry in 0..16u32 {
            let raw = index as u32 * 16 + entry;
            if raw >= 32
                && let Ok(spi) = SpiId::new(raw)
                && let Ok(interrupt) = self.interrupt_mut(spi)
            {
                let trigger = if value & (0b10 << (entry * 2)) != 0 {
                    TriggerMode::Edge
                } else {
                    TriggerMode::Level
                };
                interrupt.set_trigger(trigger);
            }
        }
        Ok(())
    }
}

fn word_index(offset: u64, base: u64, count: u64) -> Option<u64> {
    (offset >= base && offset < base + count * 4 && offset.is_multiple_of(4))
        .then_some((offset - base) / 4)
}

fn validate_access(
    offset: u64,
    width: AccessWidth,
    config: &GicV3Config,
    operation: &'static str,
) -> VgicResult {
    if offset
        .checked_add(width.size() as u64)
        .is_none_or(|end| end > config.distributor().size())
        || !offset.is_multiple_of(width.size() as u64)
    {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Distributor,
            operation,
            offset,
            width,
            detail: "access is unaligned or outside the Distributor frame".into(),
        });
    }
    Ok(())
}

fn require_width(
    offset: u64,
    actual: AccessWidth,
    expected: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    if actual != expected {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Distributor,
            operation,
            offset,
            width: actual,
            detail: alloc::format!("register requires {expected:?}"),
        });
    }
    Ok(())
}
