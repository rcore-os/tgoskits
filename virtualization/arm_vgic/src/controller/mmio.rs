//! Checked guest-visible GICv3 MMIO views.

use alloc::vec::Vec;

use axvm_types::AccessWidth;

use super::{ControllerState, GicV3Controller};
use crate::{
    GicVcpuId, ItsAction, RegisterRegion, VgicError, VgicResult,
    register::{
        GITS_BASER, GITS_BASER_COUNT, GITS_CBASER, GITS_CREADR, GITS_CTLR, GITS_CWRITER, GITS_IIDR,
        GITS_TYPER, GicComponent, component_id,
    },
};

impl GicV3Controller {
    /// Reads a Distributor register.
    pub fn read_distributor(&self, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        self.inner
            .state
            .lock()
            .distributor
            .read(offset, width, &self.inner.config)
    }

    /// Writes a Distributor register and schedules newly deliverable SPIs.
    pub fn write_distributor(&self, offset: u64, width: AccessWidth, value: u64) -> VgicResult {
        let (wakes, physical_state_changes) = {
            let mut state = self.inner.state.lock();
            let physical_snapshot = state.physical_interrupt_snapshot()?;
            let write = state
                .distributor
                .write(offset, width, value, &self.inner.config)?;
            let candidates = write.into_candidates();
            let mut wakes = Vec::new();
            for spi in candidates {
                if state.has_software_backing(spi, &self.inner.config)
                    && let Some(wake) = state.queue_spi_if_deliverable(spi)?
                {
                    wakes.push(wake);
                }
            }
            let physical_state_changes =
                state.active_physical_interrupt_state_changes(&physical_snapshot)?;
            (wakes, physical_state_changes)
        };
        self.apply_physical_interrupt_state_changes(physical_state_changes)?;
        for wake in wakes {
            wake.wake()?;
        }
        Ok(())
    }

    /// Reads one Redistributor register frame.
    pub fn read_redistributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
    ) -> VgicResult<u64> {
        self.inner
            .state
            .lock()
            .redistributor(vcpu, "read Redistributor")?
            .read(offset, width, &self.inner.config)
    }

    /// Writes one Redistributor register frame.
    pub fn write_redistributor(
        &self,
        vcpu: GicVcpuId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        let wakes = {
            let mut state = self.inner.state.lock();
            let candidates = state
                .redistributor_mut(vcpu, "write Redistributor")?
                .write(offset, width, value, &self.inner.config)?;
            let mut wakes = Vec::new();
            for intid in candidates {
                if let Some(wake) = state.queue_local_if_deliverable(vcpu, intid)? {
                    wakes.push(wake);
                }
            }
            wakes
        };
        for wake in wakes {
            wake.wake()?;
        }
        Ok(())
    }

    /// Reads a software ITS register.
    pub fn read_its(&self, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        validate_its_access(self, offset, width, "read")?;
        if let Some(value) = component_id(offset, GicComponent::Its) {
            return Ok(value);
        }
        let state = self.inner.state.lock();
        let its = &state.its;
        if let Some(base) = wide_register_base(offset) {
            let value = match base {
                GITS_TYPER => its_typer(self.inner.config.lpi_limit()),
                GITS_CBASER => its.cbaser(),
                GITS_CWRITER => its.cwriter(),
                GITS_CREADR => its.creadr(),
                _ => its.baser(baser_index(base).ok_or_else(|| VgicError::InvalidAccess {
                    region: RegisterRegion::Its,
                    operation: "read",
                    offset,
                    width,
                    detail: "wide register does not belong to an ITS register bank".into(),
                })?),
            };
            return Ok(read_wide_register(value, offset, base, width));
        }
        match offset {
            GITS_CTLR => Ok(u64::from(its.enabled()) | (1 << 31)),
            GITS_IIDR => Ok(0x43b),
            _ => Ok(0),
        }
    }

    /// Writes a software ITS register and processes bounded command work.
    pub fn write_its(&self, offset: u64, width: AccessWidth, value: u64) -> VgicResult {
        validate_its_access(self, offset, width, "write")?;
        let actions = {
            let mut state = self.inner.state.lock();
            if let Some(base) = wide_register_base(offset) {
                let current = match base {
                    GITS_TYPER => its_typer(self.inner.config.lpi_limit()),
                    GITS_CBASER => state.its.cbaser(),
                    GITS_CWRITER => state.its.cwriter(),
                    GITS_CREADR => state.its.creadr(),
                    _ => state.its.baser(baser_index(base).ok_or_else(|| {
                        VgicError::InvalidAccess {
                            region: RegisterRegion::Its,
                            operation: "write",
                            offset,
                            width,
                            detail: "wide register does not belong to an ITS register bank".into(),
                        }
                    })?),
                };
                let merged = merge_wide_register(current, offset, base, width, value);
                match base {
                    GITS_TYPER | GITS_CREADR => Vec::new(),
                    GITS_CBASER => {
                        if !state.its.enabled() {
                            state.its.set_cbaser(merged)?;
                        }
                        Vec::new()
                    }
                    GITS_CWRITER => {
                        state.its.set_cwriter(merged)?;
                        process_its_commands(self, &mut state)?
                    }
                    _ => {
                        if !state.its.enabled() {
                            let index =
                                baser_index(base).ok_or_else(|| VgicError::InvalidAccess {
                                    region: RegisterRegion::Its,
                                    operation: "write",
                                    offset,
                                    width,
                                    detail: "wide register does not belong to an ITS register bank"
                                        .into(),
                                })?;
                            state.its.set_baser(index, merged);
                        }
                        Vec::new()
                    }
                }
            } else {
                match offset {
                    GITS_CTLR => {
                        let enabled = value & 1 != 0;
                        state.its.set_enabled(enabled);
                        if enabled {
                            process_its_commands(self, &mut state)?
                        } else {
                            Vec::new()
                        }
                    }
                    GITS_IIDR => Vec::new(),
                    _ => Vec::new(),
                }
            }
        };
        self.apply_its_actions(actions)
    }

    fn apply_its_actions(&self, actions: Vec<ItsAction>) -> VgicResult {
        let wakes = {
            let mut state = self.inner.state.lock();
            let mut wakes = Vec::new();
            for action in actions {
                match action {
                    ItsAction::SetPending {
                        target,
                        lpi,
                        pending,
                    } => {
                        if let Some(wake) = state.set_lpi_pending(target, lpi, pending)? {
                            wakes.push(wake);
                        }
                    }
                }
            }
            wakes
        };
        for wake in wakes {
            wake.wake()?;
        }
        Ok(())
    }
}

fn process_its_commands(
    controller: &GicV3Controller,
    state: &mut ControllerState,
) -> VgicResult<Vec<ItsAction>> {
    if !state.its.enabled() || !state.its.has_pending_commands() {
        return Ok(Vec::new());
    }
    let memory =
        controller
            .inner
            .guest_memory
            .as_deref()
            .ok_or_else(|| VgicError::Unsupported {
                operation: "process ITS command queue",
                detail: "no guest-memory capability is installed".into(),
            })?;
    let processor_targets = state.redistributors.keys().copied().collect::<Vec<_>>();
    state.its.process_commands(
        memory,
        controller.inner.config.its_command_budget(),
        controller.inner.config.lpi_limit(),
        &processor_targets,
    )
}

fn validate_its_access(
    controller: &GicV3Controller,
    offset: u64,
    width: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    let region = controller
        .inner
        .config
        .its()
        .ok_or_else(|| VgicError::Unsupported {
            operation: "access guest ITS registers",
            detail: "this controller has no ITS frame".into(),
        })?;
    if offset
        .checked_add(width.size() as u64)
        .is_none_or(|end| end > region.size())
        || !offset.is_multiple_of(width.size() as u64)
    {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Its,
            operation,
            offset,
            width,
            detail: "access is unaligned or outside the ITS frame".into(),
        });
    }
    let valid_width = if matches!(offset, GITS_CTLR | GITS_IIDR)
        || component_id(offset, GicComponent::Its).is_some()
    {
        width == AccessWidth::Dword
    } else if let Some(base) = wide_register_base(offset) {
        width == AccessWidth::Dword || (width == AccessWidth::Qword && offset == base)
    } else {
        true
    };
    if !valid_width {
        return Err(VgicError::InvalidAccess {
            region: RegisterRegion::Its,
            operation,
            offset,
            width,
            detail: "register requires a Dword half or an aligned Qword access".into(),
        });
    }
    Ok(())
}

fn wide_register_base(offset: u64) -> Option<u64> {
    for base in [GITS_TYPER, GITS_CBASER, GITS_CWRITER, GITS_CREADR] {
        if (base..base + 8).contains(&offset) {
            return Some(base);
        }
    }
    (GITS_BASER..GITS_BASER + GITS_BASER_COUNT as u64 * 8)
        .contains(&offset)
        .then(|| GITS_BASER + (offset - GITS_BASER) / 8 * 8)
}

fn read_wide_register(value: u64, offset: u64, base: u64, width: AccessWidth) -> u64 {
    if width == AccessWidth::Qword {
        value
    } else if offset == base {
        value & u64::from(u32::MAX)
    } else {
        value >> 32
    }
}

fn merge_wide_register(
    current: u64,
    offset: u64,
    base: u64,
    width: AccessWidth,
    value: u64,
) -> u64 {
    if width == AccessWidth::Qword {
        value
    } else if offset == base {
        (current & !u64::from(u32::MAX)) | (value & u64::from(u32::MAX))
    } else {
        (current & u64::from(u32::MAX)) | ((value & u64::from(u32::MAX)) << 32)
    }
}

fn baser_index(offset: u64) -> Option<usize> {
    (GITS_BASER..GITS_BASER + GITS_BASER_COUNT as u64 * 8)
        .contains(&offset)
        .then_some(((offset - GITS_BASER) / 8) as usize)
}

fn its_typer(lpi_limit: u32) -> u64 {
    const PHYSICAL_LPIS: u64 = 1;
    const ITT_ENTRY_SIZE: u64 = 8;
    const DEVICE_ID_BITS: u64 = 16;

    let interrupt_id_bits = u64::from(u32::BITS - lpi_limit.leading_zeros());
    PHYSICAL_LPIS
        | ((ITT_ENTRY_SIZE - 1) << 4)
        | ((interrupt_id_bits - 1) << 8)
        | ((DEVICE_ID_BITS - 1) << 13)
}
