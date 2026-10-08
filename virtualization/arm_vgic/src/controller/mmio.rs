//! Checked guest-visible GICv3 MMIO views.

use alloc::{collections::BTreeMap, vec::Vec};

use axdevice_base::ItsId;
use axvm_types::AccessWidth;

use super::{ControllerConfig, GicV3Controller, GicV3Native, state::detached_lpi_target};
use crate::{
    GicVcpuId, GuestMemory, ItsAction, ItsCommandProgress, ItsState, RegisterRegion, VgicError,
    VgicResult,
    register::{
        GITS_BASER, GITS_BASER_COUNT, GITS_CBASER, GITS_CREADR, GITS_CTLR, GITS_CWRITER, GITS_IIDR,
        GITS_TYPER, GicComponent, component_id,
    },
};

impl GicV3Native {
    /// Reads a Distributor register.
    pub fn read_distributor(&self, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        self.inner
            .state
            .lock_irqsave()
            .distributor
            .read(offset, width, &self.inner.config)
    }

    /// Writes a Distributor register and schedules newly deliverable SPIs.
    pub fn write_distributor(&self, offset: u64, width: AccessWidth, value: u64) -> VgicResult {
        let (wakes, physical_state_changes) = {
            let mut state = self.inner.state.lock_irqsave();
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
            let acknowledged_physical_spis = state
                .physical_spi_acknowledged
                .iter()
                .filter_map(|(spi, acknowledged)| acknowledged.then_some(*spi))
                .collect::<Vec<_>>();
            for spi in acknowledged_physical_spis {
                if let Some(wake) = state.queue_acknowledged_physical_spi_if_deliverable(spi)? {
                    wakes.push(wake);
                }
            }
            let physical_state_changes =
                state.physical_interrupt_state_changes(&physical_snapshot)?;
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
            .lock_irqsave()
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
            let mut state = self.inner.state.lock_irqsave();
            let loaded = state.cpu_interface_loaded(vcpu);
            let candidates = state
                .redistributor_mut(vcpu, "write Redistributor")?
                .write(offset, width, value, &self.inner.config, loaded)?;
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
}

impl GicV3Controller {
    /// Reads a software ITS register.
    pub fn read_its(&self, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        self.read_its_for(ItsId::new(0), offset, width)
    }

    /// Reads a register from one software ITS instance.
    ///
    /// Register state lives in the sleepable task-side ITS service, so this
    /// path never takes the native raw state lock.
    pub fn read_its_for(&self, its_id: ItsId, offset: u64, width: AccessWidth) -> VgicResult<u64> {
        validate_its_access(&self.native.inner.config, its_id, offset, width, "read")?;
        if let Some(value) = component_id(offset, GicComponent::Its) {
            return Ok(value);
        }
        let states = self.its.states().lock();
        let its = states
            .get(&its_id)
            .ok_or_else(|| missing_its(its_id, "read ITS register"))?;
        if let Some(base) = wide_register_base(offset) {
            let value = its_wide_register(
                its,
                base,
                self.native.inner.config.lpi_limit(),
                offset,
                width,
            )?;
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
        self.write_its_for(ItsId::new(0), offset, width, value)
    }

    /// Writes a register in one software ITS instance.
    ///
    /// Command-queue copies and dynamic translation-table updates run in task
    /// context under the sleepable ITS mutex. Only pre-decoded delivery effects
    /// re-enter the native raw state, and the wake is published after both locks
    /// are released.
    ///
    /// Every buffer touched while the native raw state is locked is sized
    /// outside that guard from immutable configuration, so the locked body only
    /// copies attached identities and clones ready wake capabilities. A command
    /// that was already consumed keeps its decoded delivery effects even when a
    /// later command fails, and the failing command stays at CREADR.
    pub fn write_its_for(
        &self,
        its_id: ItsId,
        offset: u64,
        width: AccessWidth,
        value: u64,
    ) -> VgicResult {
        validate_its_access(&self.native.inner.config, its_id, offset, width, "write")?;
        // Reserve the MAPC target snapshot from immutable configuration before
        // the raw lock is taken. `attach_vcpu` rejects an id at or above
        // `vcpu_count` and rejects duplicates, so the snapshot below cannot
        // outgrow this capacity and the locked body never allocates.
        let mut processor_targets = Vec::with_capacity(self.native.inner.config.vcpu_count());
        {
            let state = self.native.inner.state.lock_irqsave();
            processor_targets.extend(state.redistributors.keys().copied());
        }
        let budget = self.native.inner.config.its_command_budget();
        let lpi_limit = self.native.inner.config.lpi_limit();
        let context = ItsCommandContext {
            memory: self.its.memory(),
            budget,
            lpi_limit,
            processor_targets: &processor_targets,
        };
        let mut states = self.its.states().lock();
        let progress = write_its_register(&mut states, its_id, offset, width, value, &context)?;
        let (actions, failure) = progress.into_parts();
        // Reserve one wake per decoded delivery effect outside the native raw
        // lock; the loop below only pushes into that capacity and never grows.
        let mut wakes = Vec::with_capacity(actions.len());
        let mut detached = None;
        // Apply pre-decoded delivery effects while the ITS lock is still held so
        // concurrent software-ITS MMIO keeps its program order on this instance.
        {
            let mut state = self.native.inner.state.lock_irqsave();
            for action in actions {
                match action {
                    ItsAction::SetPending {
                        target,
                        lpi,
                        pending,
                    } => match state.set_lpi_pending(target, lpi, pending) {
                        Ok(Some(wake)) => wakes.push(wake),
                        Ok(None) => {}
                        // The target only detaches while the VM is torn down.
                        // Keep the queue position and report after the guard.
                        Err(detached_target) => {
                            detached.get_or_insert(detached_target);
                        }
                    },
                }
            }
        }
        drop(states);
        for wake in wakes {
            wake.wake()?;
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        if let Some(target) = detached {
            return Err(detached_lpi_target(target));
        }
        Ok(())
    }
}

/// Immutable inputs shared by one software-ITS register write.
struct ItsCommandContext<'a> {
    memory: Option<&'a dyn GuestMemory>,
    budget: usize,
    lpi_limit: u32,
    processor_targets: &'a [GicVcpuId],
}

fn write_its_register(
    states: &mut BTreeMap<ItsId, ItsState>,
    its_id: ItsId,
    offset: u64,
    width: AccessWidth,
    value: u64,
    context: &ItsCommandContext<'_>,
) -> VgicResult<ItsCommandProgress> {
    if let Some(base) = wide_register_base(offset) {
        let current = its_wide_register(
            states
                .get(&its_id)
                .ok_or_else(|| missing_its(its_id, "write ITS wide register"))?,
            base,
            context.lpi_limit,
            offset,
            width,
        )?;
        let merged = merge_wide_register(current, offset, base, width, value);
        return match base {
            GITS_TYPER | GITS_CREADR => Ok(ItsCommandProgress::complete(Vec::new())),
            GITS_CBASER => {
                let its = its_state_mut(states, its_id, "write CBASER")?;
                if !its.enabled() {
                    its.set_cbaser(merged)?;
                }
                Ok(ItsCommandProgress::complete(Vec::new()))
            }
            GITS_CWRITER => {
                its_state_mut(states, its_id, "write CWRITER")?.set_cwriter(merged)?;
                process_its_commands(states, its_id, context)
            }
            _ => {
                let index = baser_index(base).ok_or_else(|| VgicError::InvalidAccess {
                    region: RegisterRegion::Its,
                    operation: "write",
                    offset,
                    width,
                    detail: "wide register does not belong to an ITS register bank".into(),
                })?;
                let its = its_state_mut(states, its_id, "write BASER")?;
                if !its.enabled() {
                    its.set_baser(index, merged);
                }
                Ok(ItsCommandProgress::complete(Vec::new()))
            }
        };
    }
    match offset {
        GITS_CTLR => {
            let enabled = value & 1 != 0;
            its_state_mut(states, its_id, "write CTLR")?.set_enabled(enabled);
            if enabled {
                process_its_commands(states, its_id, context)
            } else {
                Ok(ItsCommandProgress::complete(Vec::new()))
            }
        }
        GITS_IIDR => Ok(ItsCommandProgress::complete(Vec::new())),
        _ => Ok(ItsCommandProgress::complete(Vec::new())),
    }
}

fn process_its_commands(
    states: &mut BTreeMap<ItsId, ItsState>,
    its_id: ItsId,
    context: &ItsCommandContext<'_>,
) -> VgicResult<ItsCommandProgress> {
    let its = its_state_mut(states, its_id, "process command queue")?;
    if !its.enabled() || !its.has_pending_commands() {
        return Ok(ItsCommandProgress::complete(Vec::new()));
    }
    let memory = context.memory.ok_or_else(|| VgicError::Unsupported {
        operation: "process ITS command queue",
        detail: "no guest-memory capability is installed".into(),
    })?;
    its.process_commands(
        memory,
        context.budget,
        context.lpi_limit,
        context.processor_targets,
    )
}

fn its_state_mut<'a>(
    states: &'a mut BTreeMap<ItsId, ItsState>,
    its_id: ItsId,
    operation: &'static str,
) -> VgicResult<&'a mut ItsState> {
    states
        .get_mut(&its_id)
        .ok_or_else(|| missing_its(its_id, operation))
}

fn its_wide_register(
    its: &ItsState,
    base: u64,
    lpi_limit: u32,
    offset: u64,
    width: AccessWidth,
) -> VgicResult<u64> {
    match base {
        GITS_TYPER => Ok(its_typer(lpi_limit)),
        GITS_CBASER => Ok(its.cbaser()),
        GITS_CWRITER => Ok(its.cwriter()),
        GITS_CREADR => Ok(its.creadr()),
        _ => Ok(
            its.baser(baser_index(base).ok_or_else(|| VgicError::InvalidAccess {
                region: RegisterRegion::Its,
                operation: "access ITS register bank",
                offset,
                width,
                detail: "wide register does not belong to an ITS register bank".into(),
            })?),
        ),
    }
}

fn validate_its_access(
    config: &ControllerConfig,
    its_id: ItsId,
    offset: u64,
    width: AccessWidth,
    operation: &'static str,
) -> VgicResult {
    let region = config
        .its_instances()
        .iter()
        .find_map(|(id, region)| (*id == its_id).then_some(*region))
        .ok_or_else(|| VgicError::Unsupported {
            operation: "access guest ITS registers",
            detail: alloc::format!("this controller has no ITS {} frame", its_id.value()),
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

fn missing_its(its: ItsId, operation: &'static str) -> VgicError {
    VgicError::ResourceNotFound {
        resource: alloc::format!("ITS {}", its.value()),
        operation,
    }
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
