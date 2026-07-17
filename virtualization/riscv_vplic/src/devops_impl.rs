//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use core::sync::atomic::Ordering;

use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceAddrRange, DeviceResult, EmuDeviceType};
use axvm_types::GuestPhysAddrRange;
use bitmaps::Bitmap;

use crate::{ForwardedBatchError, VplicError, VplicResult, consts::*, vplic::VPlicGlobal};

const FORWARDED_ROUTE_BATCH_MAX: usize = 64;

fn validate_forwarded_route_batch(route_generation: u64, irq_ids: &[usize]) -> VplicResult {
    if route_generation == 0 {
        return Err(VplicError::InvalidForwardedGeneration);
    }
    if irq_ids.len() > FORWARDED_ROUTE_BATCH_MAX {
        return Err(VplicError::ForwardedBatchTooLarge {
            actual: irq_ids.len(),
            maximum: FORWARDED_ROUTE_BATCH_MAX,
        });
    }
    Ok(())
}

impl VPlicGlobal {
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

        let assigned_irqs = self.assigned_irqs.lock();
        if !assigned_irqs.is_empty() && !assigned_irqs.get(irq_id) {
            return Err(VplicError::SourceNotAssigned { source_id: irq_id });
        }
        Ok(())
    }

    fn update_pending_irq(&self, irq_id: usize, pending: bool) -> VplicResult {
        self.validate_assigned_irq(irq_id)?;
        self.pending_irqs.lock().set(irq_id, pending);
        Ok(())
    }

    /// Transfers one already claimed and masked physical source into the
    /// software PLIC pending state.
    ///
    /// The caller retains the opaque platform claim until
    /// [`Self::take_completed_forwarded_irq`] reports that the guest completed
    /// this source.
    pub fn set_forwarded_pending(&self, irq_id: usize) -> VplicResult {
        self.set_forwarded_pending_batch(core::slice::from_ref(&irq_id))
            .map_err(ForwardedBatchError::into_cause)
    }

    /// Transfers a bounded batch of physical sources into software state.
    ///
    /// Validation and publication hold each vPLIC bitmap lock once, then
    /// recompute guest context lines once for the whole batch. The operation is
    /// all-or-nothing: an invalid, duplicate, or already-forwarded source does
    /// not publish any member of the batch.
    pub fn set_forwarded_pending_batch(
        &self,
        irq_ids: &[usize],
    ) -> Result<(), ForwardedBatchError> {
        self.set_forwarded_pending_batch_for_generation(irq_ids, 1)
    }

    /// Transfers a bounded physical-source batch under one route generation.
    ///
    /// A different nonzero generation cannot reuse a source until normal
    /// completion or explicit route revocation clears the old ownership.
    pub fn set_forwarded_pending_batch_for_generation(
        &self,
        irq_ids: &[usize],
        route_generation: u64,
    ) -> Result<(), ForwardedBatchError> {
        validate_forwarded_route_batch(route_generation, irq_ids)?;
        let assigned_irqs = self.assigned_irqs.lock();
        let mut pending_irqs = self.pending_irqs.lock();
        let active_irqs = self.active_irqs.lock();
        let mut forwarded_irqs = self.forwarded_irqs.lock();
        let completed_forwarded_irqs = self.completed_forwarded_irqs.lock();
        let mut batch = Bitmap::<{ PLIC_NUM_SOURCES }>::new();
        for &irq_id in irq_ids {
            Self::validate_irq_id(irq_id)?;
            if !assigned_irqs.is_empty() && !assigned_irqs.get(irq_id) {
                return Err(VplicError::SourceNotAssigned { source_id: irq_id }.into());
            }
            if batch.get(irq_id)
                || forwarded_irqs.get(irq_id)
                || completed_forwarded_irqs.get(irq_id)
            {
                return Err(VplicError::ForwardedSourceBusy { source_id: irq_id }.into());
            }
            if pending_irqs.get(irq_id) || active_irqs.get(irq_id) {
                return Err(VplicError::ForwardedSourceCollision { source_id: irq_id }.into());
            }
            let installed = self.forwarded_route_generations[irq_id].load(Ordering::Acquire);
            if installed != 0 && installed != route_generation {
                return Err(VplicError::ForwardedGenerationMismatch {
                    source_id: irq_id,
                    expected: route_generation,
                    actual: installed,
                }
                .into());
            }
            batch.set(irq_id, true);
        }
        for irq_id in (&batch).into_iter() {
            self.forwarded_route_generations[irq_id].store(route_generation, Ordering::Release);
            pending_irqs.set(irq_id, true);
            forwarded_irqs.set(irq_id, true);
        }
        drop(completed_forwarded_irqs);
        drop(forwarded_irqs);
        drop(active_irqs);
        drop(pending_irqs);
        drop(assigned_irqs);
        self.refresh_all_guest_context_lines()
            .map_err(ForwardedBatchError::Committed)
    }

    /// Clears a bounded stopped-guest forwarding batch for one route generation.
    ///
    /// Sources with no physical forwarding owner are left unchanged, so an
    /// unrelated guest-created pending interrupt is never erased. A stale
    /// generation is rejected before any source changes.
    pub fn revoke_forwarded_route_batch(
        &self,
        route_generation: u64,
        irq_ids: &[usize],
    ) -> VplicResult<usize> {
        validate_forwarded_route_batch(route_generation, irq_ids)?;
        let mut pending_irqs = self.pending_irqs.lock();
        let mut active_irqs = self.active_irqs.lock();
        let mut forwarded_irqs = self.forwarded_irqs.lock();
        let mut completed_forwarded_irqs = self.completed_forwarded_irqs.lock();
        let mut batch = Bitmap::<{ PLIC_NUM_SOURCES }>::new();
        for &irq_id in irq_ids {
            Self::validate_irq_id(irq_id)?;
            if batch.get(irq_id) {
                return Err(VplicError::ForwardedSourceBusy { source_id: irq_id });
            }
            batch.set(irq_id, true);
            let installed = self.forwarded_route_generations[irq_id].load(Ordering::Acquire);
            if installed == 0 {
                if forwarded_irqs.get(irq_id) || completed_forwarded_irqs.get(irq_id) {
                    return Err(VplicError::ForwardedGenerationMismatch {
                        source_id: irq_id,
                        expected: route_generation,
                        actual: 0,
                    });
                }
            } else if installed != route_generation {
                return Err(VplicError::ForwardedGenerationMismatch {
                    source_id: irq_id,
                    expected: route_generation,
                    actual: installed,
                });
            }
        }

        let mut revoked = 0;
        for irq_id in (&batch).into_iter() {
            if self.forwarded_route_generations[irq_id].load(Ordering::Acquire) == 0 {
                continue;
            }
            pending_irqs.set(irq_id, false);
            active_irqs.set(irq_id, false);
            forwarded_irqs.set(irq_id, false);
            completed_forwarded_irqs.set(irq_id, false);
            self.forwarded_route_generations[irq_id].store(0, Ordering::Release);
            revoked += 1;
        }
        drop(completed_forwarded_irqs);
        drop(forwarded_irqs);
        drop(active_irqs);
        drop(pending_irqs);
        self.refresh_all_guest_context_lines()?;
        Ok(revoked)
    }

    /// Retires generation metadata after the host unmasked completed claims.
    pub fn finish_forwarded_route_batch(
        &self,
        route_generation: u64,
        irq_ids: &[usize],
    ) -> VplicResult {
        validate_forwarded_route_batch(route_generation, irq_ids)?;
        let pending_irqs = self.pending_irqs.lock();
        let active_irqs = self.active_irqs.lock();
        let forwarded_irqs = self.forwarded_irqs.lock();
        let completed_forwarded_irqs = self.completed_forwarded_irqs.lock();
        let mut batch = Bitmap::<{ PLIC_NUM_SOURCES }>::new();
        for &irq_id in irq_ids {
            Self::validate_irq_id(irq_id)?;
            if batch.get(irq_id)
                || pending_irqs.get(irq_id)
                || active_irqs.get(irq_id)
                || forwarded_irqs.get(irq_id)
                || completed_forwarded_irqs.get(irq_id)
            {
                return Err(VplicError::ForwardedSourceBusy { source_id: irq_id });
            }
            batch.set(irq_id, true);
            let installed = self.forwarded_route_generations[irq_id].load(Ordering::Acquire);
            if installed != route_generation {
                return Err(VplicError::ForwardedGenerationMismatch {
                    source_id: irq_id,
                    expected: route_generation,
                    actual: installed,
                });
            }
        }
        for irq_id in (&batch).into_iter() {
            self.forwarded_route_generations[irq_id].store(0, Ordering::Release);
        }
        Ok(())
    }

    /// Takes one source whose guest claim/complete cycle has finished.
    pub fn take_completed_forwarded_irq(&self) -> Option<usize> {
        let mut source = [0usize; 1];
        (self.take_completed_forwarded_batch(&mut source) == 1).then_some(source[0])
    }

    /// Takes at most `sources.len()` completed forwarded sources while
    /// acquiring the completion bitmap lock once.
    ///
    /// The caller controls the bounded batch capacity. Every returned source
    /// is removed exactly once from the completion bitmap before this method
    /// returns.
    pub fn take_completed_forwarded_batch(&self, sources: &mut [usize]) -> usize {
        let mut completed = self.completed_forwarded_irqs.lock();
        let snapshot = *completed;
        let mut count = 0;
        for irq_id in (&snapshot).into_iter().filter(|irq_id| *irq_id != 0) {
            if count == sources.len() {
                break;
            }
            sources[count] = irq_id;
            count += 1;
        }
        for &irq_id in &sources[..count] {
            completed.set(irq_id, false);
        }
        count
    }

    /// Returns whether a guest completion remains for the fixed platform
    /// owner without consuming it.
    pub fn has_completed_forwarded_irq(&self) -> bool {
        (&*self.completed_forwarded_irqs.lock())
            .into_iter()
            .any(|irq_id| irq_id != 0)
    }

    /// Restores a completion publication when the host platform could not yet
    /// complete and unmask its physical claim.
    pub fn restore_completed_forwarded_irq(&self, irq_id: usize) -> VplicResult {
        Self::validate_irq_id(irq_id)?;
        self.completed_forwarded_irqs.lock().set(irq_id, true);
        Ok(())
    }

    /// Marks one interrupt source as pending.
    ///
    /// Source ID 0 and IDs outside the PLIC source range are rejected. An
    /// empty assignment bitmap preserves the existing unrestricted behavior;
    /// once assignments are populated, only assigned sources are accepted.
    pub fn set_pending(&self, irq_id: usize) -> VplicResult {
        self.update_pending_irq(irq_id, true)?;
        self.refresh_all_guest_context_lines()
    }

    /// Clears the pending state of one interrupt source.
    pub fn clear_pending(&self, irq_id: usize) -> VplicResult {
        self.update_pending_irq(irq_id, false)?;
        self.refresh_all_guest_context_lines()
    }

    /// Returns whether one interrupt source is pending.
    pub fn is_pending(&self, irq_id: usize) -> VplicResult<bool> {
        self.validate_assigned_irq(irq_id)?;
        Ok(self.pending_irqs.lock().get(irq_id))
    }

    /// Returns pending interrupts that are not currently in service.
    fn pending_inactive_irqs(&self) -> Bitmap<{ PLIC_NUM_SOURCES }> {
        let pending_irqs = self.pending_irqs.lock();
        let active_irqs = self.active_irqs.lock();
        let mut candidates = *pending_irqs & !*active_irqs;
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
                cached_enable_mask = self.context_enable_word(context_id, reg_index)?;
                cached_enable_reg_index = reg_index;
            }
            if (cached_enable_mask & (1 << bit_index)) == 0 {
                continue;
            }

            let priority = self.priority(irq_id);
            if priority > best_priority {
                best_priority = priority;
                best_irq = Some((irq_id, priority));
            }
        }

        Ok(best_irq)
    }

    /// Returns the next IRQ that should assert VSEIP for this context.
    fn next_deliverable_irq(&self, context_id: usize) -> VplicResult<Option<usize>> {
        let threshold = self.context_threshold_value(context_id)?;
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
    fn claim_next_irq(&self, context_id: usize) -> VplicResult<Option<usize>> {
        loop {
            let candidate_irqs = self.pending_inactive_irqs();
            let Some((irq_id, _priority)) =
                self.best_enabled_pending_irq(context_id, candidate_irqs)?
            else {
                return Ok(None);
            };

            let mut pending_irqs = self.pending_irqs.lock();
            let mut active_irqs = self.active_irqs.lock();
            if !pending_irqs.get(irq_id) || active_irqs.get(irq_id) {
                continue;
            }

            // Claim moves the IRQ from pending to active until the guest
            // writes it back to the complete register.
            pending_irqs.set(irq_id, false);
            active_irqs.set(irq_id, true);
            return Ok(Some(irq_id));
        }
    }

    /// Recomputes the device-owned interrupt-line level for one context.
    fn refresh_context_line(&self, context_id: usize) -> VplicResult<()> {
        let asserted = self.next_deliverable_irq(context_id)?.is_some();
        self.update_context_line(context_id, asserted);
        Ok(())
    }

    /// Recomputes software line levels for all guest supervisor contexts.
    fn refresh_all_guest_context_lines(&self) -> VplicResult<()> {
        for context_id in (1..self.contexts_num).step_by(2) {
            self.refresh_context_line(context_id)?;
        }
        Ok(())
    }
}

/// Implementation of device emulation operations for virtual PLIC.
impl BaseDeviceOps<GuestPhysAddrRange> for VPlicGlobal {
    fn emu_type(&self) -> axdevice_base::EmuDeviceType {
        EmuDeviceType::PPPTGlobal
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.addr, self.size)
    }

    /// Handles MMIO read operations from the virtual PLIC.
    ///
    /// Only 32-bit (Dword) accesses are supported.
    /// Every register is backed by VM-owned software state. No guest access
    /// aliases a physical PLIC context.
    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as DeviceAddrRange>::Addr,
        width: AccessWidth,
    ) -> DeviceResult<usize> {
        let result = (|| -> VplicResult<usize> {
            if width != AccessWidth::Dword {
                return Err(VplicError::InvalidAccessWidth {
                    expected: AccessWidth::Dword,
                    actual: width,
                });
            }
            let reg = addr - self.addr;
            match reg {
                // priority
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    if !reg.is_multiple_of(4) {
                        return Err(VplicError::UnsupportedRegister {
                            operation: "read",
                            offset: reg,
                        });
                    }
                    let irq_id = (reg - PLIC_PRIORITY_OFFSET) / 4;
                    if irq_id >= PLIC_NUM_SOURCES {
                        return Ok(0);
                    }
                    Ok(self.priority(irq_id) as usize)
                }
                // pending
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                    if !(reg - PLIC_PENDING_OFFSET).is_multiple_of(4) {
                        return Err(VplicError::UnsupportedRegister {
                            operation: "read",
                            offset: reg,
                        });
                    }
                    let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                    if reg_index >= PLIC_ENABLE_WORDS {
                        return Ok(0);
                    }
                    let bit_index_start = reg_index * 32;
                    let mut val: u32 = 0;
                    let mut bit_mask: u32 = 1;
                    let pending_irqs = self.pending_irqs.lock();
                    for i in 0..32 {
                        let irq_id = bit_index_start + i as usize;
                        if irq_id != 0 && pending_irqs.get(irq_id) {
                            val |= bit_mask;
                        }
                        bit_mask <<= 1;
                    }
                    Ok(val as usize)
                }
                // enable
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let enable_offset = reg - PLIC_ENABLE_OFFSET;
                    let context_id = enable_offset / PLIC_ENABLE_STRIDE;
                    let context_offset = enable_offset % PLIC_ENABLE_STRIDE;
                    if !context_offset.is_multiple_of(4) {
                        return Err(VplicError::UnsupportedRegister {
                            operation: "read",
                            offset: reg,
                        });
                    }
                    let word = context_offset / 4;
                    Ok(self.context_enable_word(context_id, word)? as usize)
                }
                // threshold
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset - PLIC_CONTEXT_CTRL_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                    Ok(self.context_threshold_value(context_id)? as usize)
                }
                // claim/complete
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET
                        && (offset
                            - (PLIC_CONTEXT_CTRL_OFFSET + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
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
                    let Some(irq_id) = self.claim_next_irq(context_id)? else {
                        self.refresh_all_guest_context_lines()?;
                        return Ok(0);
                    };
                    // Claiming consumes one globally pending source. Recompute
                    // every guest supervisor context so another vCPU never
                    // retains a stale software line level.
                    self.refresh_all_guest_context_lines()?;
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

    /// Handles MMIO write operations to the virtual PLIC.
    ///
    /// Only 32-bit (Dword) accesses are supported.
    /// Writes update only VM-owned software state. A forwarded physical
    /// source publishes completion for the bound AxVM owner to drain through
    /// its platform capability.
    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as DeviceAddrRange>::Addr,
        width: AccessWidth,
        val: usize,
    ) -> DeviceResult {
        let result = (|| -> VplicResult {
            if width != AccessWidth::Dword {
                return Err(VplicError::InvalidAccessWidth {
                    expected: AccessWidth::Dword,
                    actual: width,
                });
            }
            let reg = addr - self.addr;
            match reg {
                // priority
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    if !reg.is_multiple_of(4) {
                        return Err(VplicError::UnsupportedRegister {
                            operation: "write",
                            offset: reg,
                        });
                    }
                    let irq_id = (reg - PLIC_PRIORITY_OFFSET) / 4;
                    if irq_id >= PLIC_NUM_SOURCES {
                        return Ok(());
                    }
                    self.set_priority(irq_id, val as u32);
                    self.refresh_all_guest_context_lines()
                }
                // pending
                // PLIC pending registers are read-only. Guest writes have no
                // effect; device sinks and the physical-forwarding owner are
                // the only producers of pending state.
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => Ok(()),
                // enable
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let enable_offset = reg - PLIC_ENABLE_OFFSET;
                    let context_id = enable_offset / PLIC_ENABLE_STRIDE;
                    let context_offset = enable_offset % PLIC_ENABLE_STRIDE;
                    if !context_offset.is_multiple_of(4) {
                        return Err(VplicError::UnsupportedRegister {
                            operation: "write",
                            offset: reg,
                        });
                    }
                    let word = context_offset / 4;
                    self.set_context_enable_word(context_id, word, val as u32)?;
                    // A mask update can instantly expose or hide already-pending IRQs.
                    self.refresh_context_line(context_id)
                }
                // threshold
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET
                        && (offset - PLIC_CONTEXT_CTRL_OFFSET)
                            .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
                {
                    let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                    self.set_context_threshold(context_id, val as u32)?;
                    // Threshold changes must be reflected on the hart line immediately.
                    self.refresh_context_line(context_id)
                }
                // claim/complete
                offset
                    if offset >= PLIC_CONTEXT_CTRL_OFFSET + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET
                        && (offset
                            - (PLIC_CONTEXT_CTRL_OFFSET + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
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
                    let irq_id = val;

                    if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                        return self.refresh_all_guest_context_lines();
                    }
                    let mut active_irqs = self.active_irqs.lock();
                    if !active_irqs.get(irq_id) {
                        drop(active_irqs);
                        return self.refresh_all_guest_context_lines();
                    }

                    active_irqs.set(irq_id, false);
                    drop(active_irqs);

                    let mut forwarded_irqs = self.forwarded_irqs.lock();
                    if forwarded_irqs.get(irq_id) {
                        forwarded_irqs.set(irq_id, false);
                        self.completed_forwarded_irqs.lock().set(irq_id, true);
                    }
                    drop(forwarded_irqs);
                    self.refresh_all_guest_context_lines()
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

#[cfg(test)]
mod tests {
    use axvm_types::GuestPhysAddr;

    use super::*;

    #[test]
    fn pending_inactive_irqs_excludes_reserved_irq_zero() {
        let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x400000), 2).unwrap();

        {
            let mut pending_irqs = vplic.pending_irqs.lock();
            pending_irqs.set(0, true);
            pending_irqs.set(1, true);
        }

        let candidates = vplic.pending_inactive_irqs();

        assert!(!candidates.get(0));
        assert!(candidates.get(1));
    }

    #[test]
    fn completed_forwarded_sources_drain_in_bounded_batches() {
        let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x400000), 2).unwrap();
        {
            let mut completed = vplic.completed_forwarded_irqs.lock();
            for source in 1..=65 {
                completed.set(source, true);
            }
        }

        let mut first = [0usize; 64];
        assert_eq!(vplic.take_completed_forwarded_batch(&mut first), 64);
        assert_eq!(first[0], 1);
        assert_eq!(first[63], 64);
        assert!(vplic.has_completed_forwarded_irq());

        let mut second = [0usize; 64];
        assert_eq!(vplic.take_completed_forwarded_batch(&mut second), 1);
        assert_eq!(second[0], 65);
        assert!(!vplic.has_completed_forwarded_irq());
    }
}
