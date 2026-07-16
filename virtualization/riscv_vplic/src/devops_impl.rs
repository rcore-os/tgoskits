//! PLIC register emulation and interrupt state transitions.

use axdevice_base::{AccessWidth, BaseDeviceOps, DeviceAddrRange, DeviceResult, EmuDeviceType};
use axvm_types::GuestPhysAddrRange;
use bitmaps::Bitmap;

use crate::{
    VplicError, VplicResult,
    consts::*,
    vplic::{VPlicGlobal, VplicState},
};

#[cfg(target_arch = "riscv64")]
const VCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
#[cfg(target_arch = "riscv64")]
const VCAUSE_VS_TIMER: usize = VCAUSE_INTERRUPT_BIT | 5;
const PLIC_PENDING_WORDS: usize = PLIC_NUM_SOURCES / 32;
const PLIC_ENABLE_WORDS: usize = PLIC_NUM_SOURCES / 32;

impl VPlicGlobal {
    /// Latches one edge-triggered source as pending.
    pub fn set_pending(&self, source_id: usize) -> VplicResult {
        {
            let mut state = self.state.lock();
            Self::validate_assigned_source(&state, source_id)?;
            state.pending.set(source_id, true);
        }
        self.sync_all_guest_contexts_vseip()
    }

    /// Updates one level-triggered source.
    ///
    /// Deasserting the electrical level does not discard an already latched
    /// request. If the source remains asserted when the guest completes it,
    /// the PLIC gateway makes it pending again.
    pub fn set_source_level(&self, source_id: usize, asserted: bool) -> VplicResult {
        {
            let mut state = self.state.lock();
            Self::validate_assigned_source(&state, source_id)?;
            state.source_levels.set(source_id, asserted);
            if asserted {
                state.pending.set(source_id, true);
            }
        }
        self.sync_all_guest_contexts_vseip()
    }

    /// Clears a latched pending request without changing its electrical level.
    pub fn clear_pending(&self, source_id: usize) -> VplicResult {
        {
            let mut state = self.state.lock();
            Self::validate_assigned_source(&state, source_id)?;
            state.pending.set(source_id, false);
        }
        self.sync_all_guest_contexts_vseip()
    }

    /// Returns whether one source has a latched pending request.
    pub fn is_pending(&self, source_id: usize) -> VplicResult<bool> {
        let state = self.state.lock();
        Self::validate_assigned_source(&state, source_id)?;
        Ok(state.pending.get(source_id))
    }

    #[cfg(test)]
    fn pending_inactive_irqs(&self) -> Bitmap<{ PLIC_NUM_SOURCES }> {
        pending_inactive_irqs(&self.state.lock())
    }

    fn claim_next_irq(&self, context_id: usize) -> VplicResult<Option<usize>> {
        let mut state = self.state.lock();
        validate_context(&state, context_id)?;
        let candidates = pending_inactive_irqs(&state);
        let Some((source_id, _priority)) = best_enabled_pending_irq(&state, context_id, candidates)
        else {
            return Ok(None);
        };
        state.pending.set(source_id, false);
        state.active.set(source_id, true);
        Ok(Some(source_id))
    }

    fn complete_irq(&self, context_id: usize, source_id: usize) -> VplicResult {
        {
            let mut state = self.state.lock();
            validate_context(&state, context_id)?;
            if source_id == 0 || source_id >= PLIC_NUM_SOURCES {
                return Ok(());
            }
            if !Self::source_is_assigned(&state, source_id) || !state.active.get(source_id) {
                return Ok(());
            }
            state.active.set(source_id, false);
            if state.source_levels.get(source_id) {
                state.pending.set(source_id, true);
            }
        }
        self.sync_all_guest_contexts_vseip()
    }

    #[cfg(target_arch = "riscv64")]
    fn sync_vseip(&self, context_id: usize) -> VplicResult {
        let deliverable = {
            let state = self.state.lock();
            validate_context(&state, context_id)?;
            next_deliverable_irq(&state, context_id).is_some()
        };
        if deliverable {
            // SAFETY: AxVM calls the PLIC from the current vCPU's trap path;
            // `hvip` therefore belongs to that bound guest context.
            unsafe {
                if riscv_h::register::vscause::read().bits() == VCAUSE_VS_TIMER {
                    riscv_h::register::hvip::clear_vstip();
                }
                riscv_h::register::hvip::set_vseip();
            }
        } else {
            // SAFETY: See the bound-current-vCPU invariant above.
            unsafe {
                riscv_h::register::hvip::clear_vseip();
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "riscv64"))]
    fn sync_vseip(&self, context_id: usize) -> VplicResult {
        validate_context(&self.state.lock(), context_id)
    }

    fn sync_all_guest_contexts_vseip(&self) -> VplicResult {
        for context_id in (1..self.context_count()).step_by(2) {
            self.sync_vseip(context_id)?;
        }
        Ok(())
    }

    fn register_offset(&self, address: axvm_types::GuestPhysAddr) -> VplicResult<usize> {
        let offset = address
            .as_usize()
            .checked_sub(self.address().as_usize())
            .filter(|offset| *offset < self.size())
            .ok_or(VplicError::UnsupportedRegister {
                operation: "access",
                offset: address.as_usize(),
            })?;
        if !offset.is_multiple_of(core::mem::size_of::<u32>()) {
            return Err(VplicError::UnalignedRegister { offset });
        }
        Ok(offset)
    }

    fn read_priority(&self, source_id: usize) -> usize {
        let state = self.state.lock();
        if source_id == 0 || !Self::source_is_assigned(&state, source_id) {
            0
        } else {
            state.priorities[source_id] as usize
        }
    }

    fn write_priority(&self, source_id: usize, priority: u32) -> VplicResult {
        if source_id == 0 {
            return Ok(());
        }
        {
            let mut state = self.state.lock();
            if !Self::source_is_assigned(&state, source_id) {
                return Ok(());
            }
            state.priorities[source_id] = priority;
        }
        self.sync_all_guest_contexts_vseip()
    }

    fn read_pending_word(&self, word: usize) -> u32 {
        let state = self.state.lock();
        bitmap_word(&state.pending, word)
    }

    fn read_enable_word(&self, context_id: usize, word: usize) -> VplicResult<u32> {
        let state = self.state.lock();
        let context = state
            .contexts
            .get(context_id)
            .ok_or(VplicError::InvalidContext {
                context: context_id,
                contexts: state.contexts.len(),
            })?;
        Ok(bitmap_word(&context.enabled, word))
    }

    fn write_enable_word(&self, context_id: usize, word: usize, value: u32) -> VplicResult {
        {
            let mut state = self.state.lock();
            validate_context(&state, context_id)?;
            for bit in 0..32 {
                let source_id = word * 32 + bit;
                if source_id >= PLIC_NUM_SOURCES {
                    break;
                }
                let enabled = source_id != 0
                    && Self::source_is_assigned(&state, source_id)
                    && value & (1 << bit) != 0;
                state.contexts[context_id].enabled.set(source_id, enabled);
            }
        }
        self.sync_vseip(context_id)
    }

    fn read_threshold(&self, context_id: usize) -> VplicResult<usize> {
        let state = self.state.lock();
        state
            .contexts
            .get(context_id)
            .map(|context| context.threshold as usize)
            .ok_or(VplicError::InvalidContext {
                context: context_id,
                contexts: state.contexts.len(),
            })
    }

    fn write_threshold(&self, context_id: usize, threshold: u32) -> VplicResult {
        {
            let mut state = self.state.lock();
            let contexts = state.contexts.len();
            let context = state
                .contexts
                .get_mut(context_id)
                .ok_or(VplicError::InvalidContext {
                    context: context_id,
                    contexts,
                })?;
            context.threshold = threshold;
        }
        self.sync_vseip(context_id)
    }
}

fn validate_context(state: &VplicState, context_id: usize) -> VplicResult {
    if context_id >= state.contexts.len() {
        return Err(VplicError::InvalidContext {
            context: context_id,
            contexts: state.contexts.len(),
        });
    }
    Ok(())
}

fn pending_inactive_irqs(state: &VplicState) -> Bitmap<{ PLIC_NUM_SOURCES }> {
    let mut candidates = state.pending & !state.active;
    candidates.set(0, false);
    candidates
}

fn best_enabled_pending_irq(
    state: &VplicState,
    context_id: usize,
    candidates: Bitmap<{ PLIC_NUM_SOURCES }>,
) -> Option<(usize, u32)> {
    let context = state.contexts.get(context_id)?;
    let mut best_irq = None;
    let mut best_priority = 0;
    for source_id in &candidates {
        if !context.enabled.get(source_id) {
            continue;
        }
        let priority = state.priorities[source_id];
        if priority > best_priority {
            best_priority = priority;
            best_irq = Some(source_id);
        }
    }
    best_irq.map(|source_id| (source_id, best_priority))
}

#[cfg(target_arch = "riscv64")]
fn next_deliverable_irq(state: &VplicState, context_id: usize) -> Option<usize> {
    let context = state.contexts.get(context_id)?;
    let candidates = pending_inactive_irqs(state);
    best_enabled_pending_irq(state, context_id, candidates)
        .filter(|(_, priority)| *priority > context.threshold)
        .map(|(source_id, _)| source_id)
}

fn bitmap_word(bitmap: &Bitmap<{ PLIC_NUM_SOURCES }>, word: usize) -> u32 {
    let mut value = 0;
    for bit in 0..32 {
        let source_id = word * 32 + bit;
        if source_id < PLIC_NUM_SOURCES && bitmap.get(source_id) {
            value |= 1 << bit;
        }
    }
    value
}

fn enable_register(offset: usize) -> (usize, usize) {
    let relative = offset - PLIC_ENABLE_OFFSET;
    (
        relative / PLIC_ENABLE_STRIDE,
        (relative % PLIC_ENABLE_STRIDE) / 4,
    )
}

fn context_register(offset: usize) -> (usize, usize) {
    let relative = offset - PLIC_CONTEXT_CTRL_OFFSET;
    (
        relative / PLIC_CONTEXT_STRIDE,
        relative % PLIC_CONTEXT_STRIDE,
    )
}

impl BaseDeviceOps<GuestPhysAddrRange> for VPlicGlobal {
    fn emu_type(&self) -> EmuDeviceType {
        EmuDeviceType::PPPTGlobal
    }

    fn address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.address(), self.size())
    }

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
            let offset = self.register_offset(addr)?;
            match offset {
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    Ok(self.read_priority(offset / core::mem::size_of::<u32>()))
                }
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                    let word = (offset - PLIC_PENDING_OFFSET) / 4;
                    if word < PLIC_PENDING_WORDS {
                        Ok(self.read_pending_word(word) as usize)
                    } else {
                        Ok(0)
                    }
                }
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let (context_id, word) = enable_register(offset);
                    if word >= PLIC_ENABLE_WORDS {
                        return Ok(0);
                    }
                    Ok(self.read_enable_word(context_id, word)? as usize)
                }
                PLIC_CONTEXT_CTRL_OFFSET.. => {
                    let (context_id, register) = context_register(offset);
                    match register {
                        PLIC_CONTEXT_THRESHOLD_OFFSET => self.read_threshold(context_id),
                        PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET => {
                            let claimed = self.claim_next_irq(context_id)?.unwrap_or(0);
                            self.sync_vseip(context_id)?;
                            Ok(claimed)
                        }
                        _ => Err(VplicError::UnsupportedRegister {
                            operation: "read",
                            offset,
                        }),
                    }
                }
            }
        })();
        Ok(result?)
    }

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
            let offset = self.register_offset(addr)?;
            match offset {
                PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                    self.write_priority(offset / core::mem::size_of::<u32>(), val as u32)
                }
                // Pending bits are read-only in the PLIC programming model.
                PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => Ok(()),
                PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                    let (context_id, word) = enable_register(offset);
                    if word >= PLIC_ENABLE_WORDS {
                        return Ok(());
                    }
                    self.write_enable_word(context_id, word, val as u32)
                }
                PLIC_CONTEXT_CTRL_OFFSET.. => {
                    let (context_id, register) = context_register(offset);
                    match register {
                        PLIC_CONTEXT_THRESHOLD_OFFSET => {
                            self.write_threshold(context_id, val as u32)
                        }
                        PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET => self.complete_irq(context_id, val),
                        _ => Err(VplicError::UnsupportedRegister {
                            operation: "write",
                            offset,
                        }),
                    }
                }
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
        vplic.set_pending(1).unwrap();

        let candidates = vplic.pending_inactive_irqs();

        assert!(!candidates.get(0));
        assert!(candidates.get(1));
    }
}
