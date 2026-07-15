//! Checked RD and SGI frame register access.

use alloc::vec::Vec;

use axvm_types::AccessWidth;

use super::RedistributorState;
use crate::{
    GicV3Config, IntId, InterruptRecord, Priority, RegisterRegion, TriggerMode, VgicError,
    VgicResult,
    register::{
        GICD_ICACTIVER, GICD_ICENABLER, GICD_ICFGR, GICD_ICPENDR, GICD_IGROUPR, GICD_IPRIORITYR,
        GICD_ISACTIVER, GICD_ISENABLER, GICD_ISPENDR, GICR_CTLR, GICR_IIDR, GICR_PENDBASER,
        GICR_PROPBASER, GICR_SGI_BASE, GICR_SYNCR, GICR_TYPER, GICR_WAKER, GicComponent,
        component_id,
    },
};

impl RedistributorState {
    pub(crate) fn read(
        &self,
        offset: u64,
        width: AccessWidth,
        config: &GicV3Config,
    ) -> VgicResult<u64> {
        validate_access(offset, width, config, "read")?;
        if offset < GICR_SGI_BASE {
            return self.read_rd_frame(offset, width, config);
        }
        self.read_sgi_frame(offset - GICR_SGI_BASE, width, config)
    }

    pub(crate) fn write(
        &mut self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        config: &GicV3Config,
    ) -> VgicResult<Vec<IntId>> {
        validate_access(offset, width, config, "write")?;
        if offset < GICR_SGI_BASE {
            return self.write_rd_frame(offset, width, value, config);
        }
        self.write_sgi_frame(offset - GICR_SGI_BASE, width, value, config)
    }

    fn read_rd_frame(
        &self,
        offset: u64,
        width: AccessWidth,
        config: &GicV3Config,
    ) -> VgicResult<u64> {
        if let Some(value) = component_id(offset, GicComponent::Redistributor) {
            require_width(offset, width, AccessWidth::Dword, "read")?;
            return Ok(value);
        }
        match offset {
            GICR_CTLR => read_dword(
                offset,
                width,
                u64::from(config.exposes_guest_lpis() && self.lpis_enabled),
            ),
            GICR_IIDR => read_dword(offset, width, 0x43b),
            GICR_TYPER => {
                require_width(offset, width, AccessWidth::Qword, "read")?;
                Ok(self.typer(config))
            }
            GICR_WAKER | GICR_SYNCR => read_dword(offset, width, 0),
            GICR_PROPBASER => {
                require_width(offset, width, AccessWidth::Qword, "read")?;
                Ok(if config.exposes_guest_lpis() {
                    self.propbaser
                } else {
                    0
                })
            }
            GICR_PENDBASER => {
                require_width(offset, width, AccessWidth::Qword, "read")?;
                Ok(if config.exposes_guest_lpis() {
                    self.pendbaser
                } else {
                    0
                })
            }
            _ => Ok(0),
        }
    }

    fn write_rd_frame(
        &mut self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        config: &GicV3Config,
    ) -> VgicResult<Vec<IntId>> {
        if component_id(offset, GicComponent::Redistributor).is_some() {
            require_width(offset, width, AccessWidth::Dword, "write")?;
            return Ok(Vec::new());
        }
        let mut candidates = Vec::new();
        match offset {
            GICR_CTLR => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                if !config.exposes_guest_lpis() {
                    return Ok(candidates);
                }
                self.lpis_enabled = value & 1 != 0;
                for interrupt in self.lpis.values_mut() {
                    interrupt.set_enabled(self.lpis_enabled);
                    if interrupt.deliverable() {
                        candidates.push(interrupt.intid());
                    }
                }
            }
            GICR_WAKER | GICR_SYNCR => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
            }
            GICR_PROPBASER => {
                require_width(offset, width, AccessWidth::Qword, "write")?;
                if config.exposes_guest_lpis() {
                    self.propbaser = value;
                }
            }
            GICR_PENDBASER => {
                require_width(offset, width, AccessWidth::Qword, "write")?;
                if config.exposes_guest_lpis() {
                    self.pendbaser = value;
                }
            }
            _ => {}
        }
        Ok(candidates)
    }

    fn read_sgi_frame(
        &self,
        offset: u64,
        width: AccessWidth,
        config: &GicV3Config,
    ) -> VgicResult<u64> {
        let owned = u64::from(config.guest_private_interrupts().raw());
        match offset {
            GICD_IGROUPR => read_dword(offset, width, owned),
            GICD_ISENABLER | GICD_ICENABLER => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(self.read_private_flags(InterruptRecord::enabled) & owned)
            }
            GICD_ISPENDR | GICD_ICPENDR => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(self.read_private_flags(InterruptRecord::pending) & owned)
            }
            GICD_ISACTIVER | GICD_ICACTIVER => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(self.read_private_flags(InterruptRecord::active) & owned)
            }
            _ if (GICD_IPRIORITYR..GICD_IPRIORITYR + 32).contains(&offset) => {
                self.read_priorities(offset, width, config)
            }
            _ if offset == GICD_ICFGR || offset == GICD_ICFGR + 4 => {
                require_width(offset, width, AccessWidth::Dword, "read")?;
                Ok(self.read_configuration(offset, config))
            }
            _ => Ok(0),
        }
    }

    fn write_sgi_frame(
        &mut self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        config: &GicV3Config,
    ) -> VgicResult<Vec<IntId>> {
        let owned = u64::from(config.guest_private_interrupts().raw());
        let mut candidates = Vec::new();
        match offset {
            GICD_IGROUPR => require_width(offset, width, AccessWidth::Dword, "write")?,
            GICD_ISENABLER => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_private_flags(value & owned, PrivateFlag::Enable)?;
            }
            GICD_ICENABLER => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_private_flags(value & owned, PrivateFlag::Disable)?;
            }
            GICD_ISPENDR => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_private_flags(value & owned, PrivateFlag::SetPending)?;
            }
            GICD_ICPENDR => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_private_flags(value & owned, PrivateFlag::ClearPending)?;
            }
            GICD_ISACTIVER => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_private_flags(value & owned, PrivateFlag::SetActive)?;
            }
            GICD_ICACTIVER => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                candidates = self.write_private_flags(value & owned, PrivateFlag::Complete)?;
            }
            _ if (GICD_IPRIORITYR..GICD_IPRIORITYR + 32).contains(&offset) => {
                self.write_priorities(offset, width, value, config)?;
            }
            _ if offset == GICD_ICFGR || offset == GICD_ICFGR + 4 => {
                require_width(offset, width, AccessWidth::Dword, "write")?;
                self.write_configuration(offset, value, config);
            }
            _ => {}
        }
        Ok(candidates)
    }

    fn typer(&self, config: &GicV3Config) -> u64 {
        let affinity = self.affinity;
        let packed_affinity = ((affinity.aff3() as u64) << 24)
            | ((affinity.aff2() as u64) << 16)
            | ((affinity.aff1() as u64) << 8)
            | affinity.aff0() as u64;
        let processor_number = (self.vcpu.raw() as u64) << 8;
        let last = u64::from(self.vcpu.raw() + 1 == config.vcpu_count()) << 4;
        let physical_lpis = u64::from(config.exposes_guest_lpis());
        (packed_affinity << 32) | processor_number | last | physical_lpis
    }

    fn read_private_flags(&self, predicate: impl Fn(&InterruptRecord) -> bool) -> u64 {
        self.private_interrupts
            .iter()
            .enumerate()
            .fold(0, |value, (bit, interrupt)| {
                value | (u64::from(predicate(interrupt)) << bit)
            })
    }

    fn write_private_flags(
        &mut self,
        value: u64,
        operation: PrivateFlag,
    ) -> VgicResult<Vec<IntId>> {
        let mut candidates = Vec::new();
        for bit in 0..32usize {
            if value & (1 << bit) == 0 {
                continue;
            }
            let intid = IntId::new(bit as u32)?;
            operation.apply(&mut self.private_interrupts[bit]);
            if matches!(operation, PrivateFlag::ClearPending) && self.clear_pending_delivery(intid)
            {
                self.private_interrupts[bit].cancel_inflight();
            }
            if self.private_interrupts[bit].deliverable() {
                candidates.push(intid);
            }
        }
        Ok(candidates)
    }

    fn read_priorities(
        &self,
        offset: u64,
        width: AccessWidth,
        config: &GicV3Config,
    ) -> VgicResult<u64> {
        validate_priority_access(offset, width, "read")?;
        let first = (offset - GICD_IPRIORITYR) as usize;
        Ok((0..width.size()).fold(0, |value, byte| {
            let raw = first + byte;
            let priority = if config.guest_private_interrupts().raw() & (1 << raw) != 0 {
                self.private_interrupts[raw].priority().raw()
            } else {
                0
            };
            value | ((priority as u64) << (byte * 8))
        }))
    }

    fn write_priorities(
        &mut self,
        offset: u64,
        width: AccessWidth,
        value: u64,
        config: &GicV3Config,
    ) -> VgicResult {
        validate_priority_access(offset, width, "write")?;
        let first = (offset - GICD_IPRIORITYR) as usize;
        for byte in 0..width.size() {
            let raw = first + byte;
            if config.guest_private_interrupts().raw() & (1 << raw) != 0 {
                self.private_interrupts[raw]
                    .set_priority(Priority::new((value >> (byte * 8)) as u8));
            }
        }
        Ok(())
    }

    fn read_configuration(&self, offset: u64, config: &GicV3Config) -> u64 {
        let first = ((offset - GICD_ICFGR) / 4) as usize * 16;
        (0..16usize).fold(0, |value, entry| {
            let raw = first + entry;
            let edge = config.guest_private_interrupts().raw() & (1 << raw) != 0
                && self.private_interrupts[raw].trigger() == TriggerMode::Edge;
            value | (u64::from(edge) << (entry * 2 + 1))
        })
    }

    fn write_configuration(&mut self, offset: u64, value: u64, config: &GicV3Config) {
        if offset == GICD_ICFGR {
            return;
        }
        for entry in 0..16usize {
            let raw = 16 + entry;
            if config.guest_private_interrupts().raw() & (1 << raw) == 0 {
                continue;
            }
            let trigger = if value & (0b10 << (entry * 2)) == 0 {
                TriggerMode::Level
            } else {
                TriggerMode::Edge
            };
            self.private_interrupts[raw].set_trigger(trigger);
        }
    }
}

#[derive(Clone, Copy)]
enum PrivateFlag {
    Enable,
    Disable,
    SetPending,
    ClearPending,
    SetActive,
    Complete,
}

impl PrivateFlag {
    fn apply(self, interrupt: &mut InterruptRecord) {
        match self {
            Self::Enable if matches!(interrupt.intid(), IntId::Ppi(_)) => {
                interrupt.set_enabled(true);
            }
            Self::Disable if matches!(interrupt.intid(), IntId::Ppi(_)) => {
                interrupt.set_enabled(false);
            }
            Self::SetPending => interrupt.set_pending(true),
            Self::ClearPending => interrupt.set_pending(false),
            Self::SetActive => interrupt.set_active(true),
            Self::Complete => interrupt.complete(),
            Self::Enable | Self::Disable => {}
        }
    }
}

fn read_dword(offset: u64, width: AccessWidth, value: u64) -> VgicResult<u64> {
    require_width(offset, width, AccessWidth::Dword, "read")?;
    Ok(value)
}

fn validate_access(
    offset: u64,
    width: AccessWidth,
    config: &GicV3Config,
    operation: &'static str,
) -> VgicResult {
    if offset
        .checked_add(width.size() as u64)
        .is_none_or(|end| end > config.redistributor_stride())
        || !offset.is_multiple_of(width.size() as u64)
    {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Redistributor,
            operation,
            offset,
            width,
            detail: "access is unaligned or outside the Redistributor frame".into(),
        });
    }
    Ok(())
}

fn validate_priority_access(
    offset: u64,
    width: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    if offset + width.size() as u64 > GICD_IPRIORITYR + 32 {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Redistributor,
            operation,
            offset: GICR_SGI_BASE + offset,
            width,
            detail: "priority access crosses the private interrupt array".into(),
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
            region: RegisterRegion::Redistributor,
            operation,
            offset,
            width: actual,
            detail: alloc::format!("register requires {expected:?}"),
        });
    }
    Ok(())
}
