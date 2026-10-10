//! Device emulation operations for VPlicGlobal.
//!
//! Implements V3 device-access handling for MMIO read/write operations.

use axdevice_base::{
    AccessWidth, BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult,
};
use axvm_types::GuestPhysAddr;
use bitmaps::Bitmap;

use crate::{
    VplicError, VplicResult,
    consts::*,
    vplic::{VPlicGlobal, VplicCompletion, VplicState},
};

const PLIC_PENDING_WORDS: usize = PLIC_NUM_SOURCES / 32;

impl VplicState {
    fn validate_irq_id(irq_id: usize) -> VplicResult {
        if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
            return Err(VplicError::InvalidSource {
                source_id: irq_id,
                max: PLIC_NUM_SOURCES,
            });
        }
        Ok(())
    }

    fn validate_assigned_irq(&self, irq_id: usize) -> VplicResult {
        Self::validate_irq_id(irq_id)?;

        if !self.assigned_irqs.is_empty() && !self.assigned_irqs.get(irq_id) {
            return Err(VplicError::SourceNotAssigned { source_id: irq_id });
        }
        Ok(())
    }

    fn update_pending_irq(&mut self, irq_id: usize, pending: bool) -> VplicResult {
        self.validate_assigned_irq(irq_id)?;
        self.pending_irqs.set(irq_id, pending);
        Ok(())
    }

    /// Marks one interrupt source as pending.
    ///
    /// Source ID 0 and IDs outside the PLIC source range are rejected. An
    /// empty assignment bitmap preserves the existing unrestricted behavior;
    /// once assignments are populated, only assigned sources are accepted.
    fn set_pending(&mut self, irq_id: usize) -> VplicResult {
        self.update_pending_irq(irq_id, true)
    }

    /// Clears the pending state of one interrupt source.
    fn clear_pending(&mut self, irq_id: usize) -> VplicResult {
        self.update_pending_irq(irq_id, false)
    }

    /// Updates one level-triggered device input.
    ///
    /// Returns `true` when a low-to-high transition needs initial delivery.
    /// The asserted state remains controller-owned so completion can repend
    /// the source until the device lowers the line.
    fn set_irq_line_level(&mut self, irq_id: usize, asserted: bool) -> VplicResult<bool> {
        self.validate_assigned_irq(irq_id)?;
        let was_asserted = self.line_asserted_irqs.get(irq_id);
        self.line_asserted_irqs.set(irq_id, asserted);
        let newly_asserted = asserted && !was_asserted;
        self.pending_irqs.set(irq_id, asserted);
        Ok(newly_asserted)
    }

    /// Returns whether one interrupt source is pending.
    fn is_pending(&self, irq_id: usize) -> VplicResult<bool> {
        self.validate_assigned_irq(irq_id)?;
        Ok(self.pending_irqs.get(irq_id))
    }

    /// Reads the priority programmed by this guest.
    fn irq_priority(&self, irq_id: usize) -> VplicResult<u32> {
        Ok(self.registers.priorities[irq_id])
    }

    /// Reads the priority threshold configured for a PLIC context.
    fn context_threshold(&self, context_id: usize) -> VplicResult<u32> {
        Ok(self.registers.thresholds[context_id])
    }

    /// Reads one enable register word for a PLIC context.
    fn context_enable_mask(&self, context_id: usize, reg_index: usize) -> VplicResult<u32> {
        Ok(self.registers.enable_masks[context_id][reg_index])
    }

    /// Returns pending interrupts that are not currently in service.
    fn pending_inactive_irqs(&self) -> Bitmap<{ PLIC_NUM_SOURCES }> {
        let mut candidates = self.pending_irqs & !self.active_irqs;
        // IRQ 0 is reserved by the PLIC specification and must never be claimed.
        candidates.set(0, false);
        candidates
    }

    /// Selects the highest-priority enabled IRQ from the candidate set.
    fn best_enabled_pending_irq(
        &self,
        context_id: usize,
        candidate_irqs: Bitmap<{ PLIC_NUM_SOURCES }>,
    ) -> VplicResult<Option<(usize, u32)>> {
        let mut best_irq = None;
        let mut best_priority = 0;
        let mut cached_enable_reg_index = usize::MAX;
        let mut cached_enable_mask = 0u32;

        // Select the highest-priority IRQ that is pending, inactive, and
        // enabled for this context. Threshold filtering is applied separately
        // for interrupt notification, but not for claim.
        for irq_id in (&candidate_irqs).into_iter() {
            let reg_index = irq_id / 32;
            let bit_index = irq_id % 32;

            if reg_index != cached_enable_reg_index {
                cached_enable_mask = self.context_enable_mask(context_id, reg_index)?;
                cached_enable_reg_index = reg_index;
            }
            if (cached_enable_mask & (1 << bit_index)) == 0 {
                continue;
            }

            let priority = self.irq_priority(irq_id)?;
            if priority > best_priority {
                best_priority = priority;
                best_irq = Some((irq_id, priority));
            }
        }

        Ok(best_irq)
    }

    /// Returns the next IRQ that should assert VSEIP for this context.
    fn next_deliverable_irq(&self, context_id: usize) -> VplicResult<Option<usize>> {
        let threshold = self.context_threshold(context_id)?;
        let candidate_irqs = self.pending_inactive_irqs();
        if let Some((irq_id, priority)) =
            self.best_enabled_pending_irq(context_id, candidate_irqs)?
            && priority > threshold
        {
            return Ok(Some(irq_id));
        }
        Ok(None)
    }

    /// Claims the next enabled pending IRQ and moves it to the active set.
    fn claim_next_irq(&mut self, context_id: usize) -> VplicResult<Option<usize>> {
        loop {
            let candidate_irqs = self.pending_inactive_irqs();
            let Some((irq_id, _priority)) =
                self.best_enabled_pending_irq(context_id, candidate_irqs)?
            else {
                return Ok(None);
            };

            if !self.pending_irqs.get(irq_id) || self.active_irqs.get(irq_id) {
                continue;
            }

            // Claim moves the IRQ from pending to active until the guest
            // writes it back to the complete register.
            self.pending_irqs.set(irq_id, false);
            self.active_irqs.set(irq_id, true);
            return Ok(Some(irq_id));
        }
    }
}

impl VPlicGlobal {
    /// Marks one interrupt source as pending through one owner transaction.
    pub fn set_pending(&self, irq_id: usize) -> VplicResult {
        self.with_state_mut(|state| state.set_pending(irq_id))
    }

    /// Clears one pending interrupt source through one owner transaction.
    pub fn clear_pending(&self, irq_id: usize) -> VplicResult {
        self.with_state_mut(|state| state.clear_pending(irq_id))
    }

    /// Updates one level-triggered input without splitting line and pending
    /// updates across independent locks.
    pub fn set_irq_line_level(&self, irq_id: usize, asserted: bool) -> VplicResult<bool> {
        self.with_state_mut(|state| state.set_irq_line_level(irq_id, asserted))
    }

    /// Returns whether one source is pending.
    pub fn is_pending(&self, irq_id: usize) -> VplicResult<bool> {
        self.with_state(|state| state.is_pending(irq_id))
    }

    /// Returns whether one context has a deliverable interrupt.
    pub fn context_has_deliverable_irq(&self, context_id: usize) -> VplicResult<bool> {
        if context_id >= self.contexts_num {
            return Err(VplicError::InvalidContext {
                context: context_id,
                contexts: self.contexts_num,
            });
        }
        self.with_state(|state| {
            state
                .next_deliverable_irq(context_id)
                .map(|irq| irq.is_some())
        })
    }
}

impl VPlicGlobal {
    fn contains(&self, addr: GuestPhysAddr) -> bool {
        let base = self.addr.as_usize();
        let end = base.saturating_add(self.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    /// Reads a virtual PLIC MMIO register.
    ///
    /// Only 32-bit (Dword) accesses are supported.
    /// Read operations are forwarded to the host PLIC for most registers,
    /// except for pending and claim/complete registers which are emulated.
    pub fn read_register(&self, addr: GuestPhysAddr, width: AccessWidth) -> DeviceResult<usize> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let result = (|| -> VplicResult<usize> {
            if width != AccessWidth::Dword {
                return Err(VplicError::InvalidAccessWidth {
                    expected: AccessWidth::Dword,
                    actual: width,
                });
            }
            let reg = addr - self.addr;
            let mut state = self.state.lock();
            // info!("vPlicGlobal read reg {reg:#x} width {width:?}");
            match reg {
                // priority
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    Ok(state.registers.priorities[reg / 4] as usize)
                }
                // pending
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                    let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                    if reg_index >= PLIC_PENDING_WORDS {
                        return Ok(0);
                    }
                    let bit_index_start = reg_index * 32;
                    let mut val: u32 = 0;
                    let mut bit_mask: u32 = 1;
                    for i in 0..32 {
                        let irq_id = bit_index_start + i as usize;
                        if irq_id != 0 && state.pending_irqs.get(irq_id) {
                            val |= bit_mask;
                        }
                        bit_mask <<= 1;
                    }
                    Ok(val as usize)
                }
                // enable
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                    let reg_index = ((reg - PLIC_ENABLE_OFFSET) % PLIC_ENABLE_STRIDE) / 4;
                    if context_id >= self.contexts_num || reg_index >= PLIC_PENDING_WORDS {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    Ok(state.registers.enable_masks[context_id][reg_index] as usize)
                }
                // threshold
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset - PLIC_CONTEXT_CTRL_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                    if context_id >= self.contexts_num {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    Ok(state.registers.thresholds[context_id] as usize)
                }
                // claim/complete
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset
                            - PLIC_CONTEXT_CTRL_OFFSET
                            - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    let context_id =
                        (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                            / PLIC_CONTEXT_STRIDE;
                    if context_id >= self.contexts_num {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    let Some(irq_id) = state.claim_next_irq(context_id)? else {
                        return Ok(0);
                    };
                    Ok(irq_id)
                }
                _ => Err(VplicError::UnsupportedRegister {
                    operation: "read",
                    offset: reg,
                }),
            }
        })();
        Ok(result?)
    }

    /// Writes a virtual PLIC MMIO register.
    ///
    /// Only 32-bit (Dword) accesses are supported.
    /// Write operations are forwarded to the host PLIC for most registers.
    /// Writes to the pending register are used for interrupt injection by the hypervisor.
    /// Writes to the claim/complete register complete interrupt handling.
    pub fn write_register(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        self.write_register_with_completion(addr, width, val)
            .map(|_| ())
    }

    /// Writes a virtual PLIC register and reports a completed active source.
    ///
    /// The controller performs the complete and level re-pend transition
    /// before returning. Any physical-backing action therefore runs after all
    /// vPLIC locks are released.
    pub fn write_register_with_completion(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult<Option<VplicCompletion>> {
        if !self.contains(addr) {
            return Err(DeviceError::OutOfRange {
                addr: addr.as_usize() as u64,
            });
        }
        let result = (|| -> VplicResult<Option<VplicCompletion>> {
            if width != AccessWidth::Dword {
                return Err(VplicError::InvalidAccessWidth {
                    expected: AccessWidth::Dword,
                    actual: width,
                });
            }
            let reg = addr - self.addr;
            let mut state = self.state.lock();
            // info!("vPlicGlobal write reg {reg:#x} width {width:?} val {val:#x}");
            match reg {
                // priority
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    state.registers.priorities[reg / 4] = val as u32;
                    Ok(None)
                }
                // pending (Here is uesd for hyperivosr to inject pending IRQs, later should move it to a separate interface)
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                    // Note: here append, not overwrite.
                    let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                    if reg_index >= PLIC_PENDING_WORDS {
                        return Ok(None);
                    }
                    let val = val as u32;
                    let mut bit_mask: u32 = 1;
                    for i in 0..32 {
                        if (val & bit_mask) != 0 {
                            let irq_id = reg_index * 32 + i;
                            if irq_id != 0 {
                                state.update_pending_irq(irq_id, true)?;
                            }
                        }
                        bit_mask <<= 1;
                    }
                    Ok(None)
                }
                // enable
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                    let reg_index = ((reg - PLIC_ENABLE_OFFSET) % PLIC_ENABLE_STRIDE) / 4;
                    if context_id >= self.contexts_num || reg_index >= PLIC_PENDING_WORDS {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    state.registers.enable_masks[context_id][reg_index] = val as u32;
                    Ok(None)
                }
                // threshold
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset - PLIC_CONTEXT_CTRL_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                    if context_id >= self.contexts_num {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    state.registers.thresholds[context_id] = val as u32;
                    Ok(None)
                }
                // claim/complete
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset
                            - PLIC_CONTEXT_CTRL_OFFSET
                            - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    // info!("vPlicGlobal: Writing to CLAIM/COMPLETE reg {reg:#x} val {val:#x}");
                    let context_id =
                        (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                            / PLIC_CONTEXT_STRIDE;
                    if context_id >= self.contexts_num {
                        return Err(VplicError::InvalidContext {
                            context: context_id,
                            contexts: self.contexts_num,
                        });
                    }
                    let irq_id = val;

                    if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                        return Ok(None);
                    }
                    if !state.active_irqs.get(irq_id) {
                        return Ok(None);
                    }

                    // Completion belongs to the virtual controller. An
                    // optional host transaction is reported only after this
                    // canonical transition and every controller lock release.
                    state.active_irqs.set(irq_id, false);
                    if state.line_asserted_irqs.get(irq_id) {
                        state.pending_irqs.set(irq_id, true);
                    }
                    Ok(Some(VplicCompletion::new(irq_id)))
                }
                _ => Err(VplicError::UnsupportedRegister {
                    operation: "write",
                    offset: reg,
                }),
            }
        })();
        Ok(result?)
    }
}

impl Device for VPlicGlobal {
    fn name(&self) -> &str {
        "riscv-vplic"
    }

    fn resources(&self) -> &[axdevice_base::Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        self.read_register(
            GuestPhysAddr::from_usize(access.address() as usize),
            access.width(),
        )
        .map(|value| value as u64)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        self.write_register(
            GuestPhysAddr::from_usize(access.address() as usize),
            access.width(),
            value as usize,
        )
    }
}

#[cfg(test)]
mod tests {
    use axvm_types::GuestPhysAddr;

    use super::*;

    #[test]
    fn pending_inactive_irqs_excludes_reserved_irq_zero() {
        let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x400000), 2).unwrap();

        vplic.with_state_mut(|state| {
            state.pending_irqs.set(0, true);
            state.pending_irqs.set(1, true);
        });
        let candidates = vplic.with_state(|state| state.pending_inactive_irqs());

        assert!(!candidates.get(0));
        assert!(candidates.get(1));
    }
}
