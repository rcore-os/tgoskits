//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddrRange, HostPhysAddr, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};
use bitmaps::Bitmap;

use crate::{consts::*, utils::*, vplic::VPlicGlobal};

const VCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
const VCAUSE_VS_TIMER: usize = VCAUSE_INTERRUPT_BIT | 5;
const PLIC_PENDING_WORDS: usize = PLIC_NUM_SOURCES / 32;

impl VPlicGlobal {
    /// Reads the priority of an interrupt source from the host PLIC.
    fn irq_priority(&self, irq_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize() + PLIC_PRIORITY_OFFSET + irq_id * 4,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    /// Reads the priority threshold configured for a PLIC context.
    fn context_threshold(&self, context_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_CONTEXT_CTRL_OFFSET
                + context_id * PLIC_CONTEXT_STRIDE
                + PLIC_CONTEXT_THRESHOLD_OFFSET,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    /// Reads one enable register word for a PLIC context.
    fn context_enable_mask(&self, context_id: usize, reg_index: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_ENABLE_OFFSET
                + context_id * PLIC_ENABLE_STRIDE
                + reg_index * 4,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
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
    ) -> AxResult<Option<(usize, u32)>> {
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
    fn next_deliverable_irq(&self, context_id: usize) -> AxResult<Option<usize>> {
        let threshold = self.context_threshold(context_id)?;
        let candidate_irqs = self.pending_inactive_irqs();
        if let Some((irq_id, priority)) =
            self.best_enabled_pending_irq(context_id, candidate_irqs)?
        {
            if priority > threshold {
                return Ok(Some(irq_id));
            }
        }
        Ok(None)
    }

    /// Claims the next enabled pending IRQ and moves it to the active set.
    fn claim_next_irq(&self, context_id: usize) -> AxResult<Option<usize>> {
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

    /// Recomputes whether VSEIP should remain asserted for one context.
    fn sync_vseip(&self, context_id: usize) -> AxResult<()> {
        // VSEIP should track whether this context still has a deliverable
        // external interrupt, not merely whether some pending bit is set.
        if self.next_deliverable_irq(context_id)?.is_some() {
            unsafe {
                // If the guest is already executing a VS timer interrupt handler,
                // the corresponding tick is "in service" from the guest's point of
                // view. Clearing VSTIP here avoids needlessly keeping a timer
                // interrupt pending while we queue the external interrupt.
                if riscv_h::register::vscause::read().bits() == VCAUSE_VS_TIMER {
                    riscv_h::register::hvip::clear_vstip();
                }
                riscv_h::register::hvip::set_vseip();
            }
        } else {
            unsafe {
                riscv_h::register::hvip::clear_vseip();
            }
        }
        Ok(())
    }

    /// Recomputes VSEIP for all guest supervisor contexts.
    fn sync_all_guest_contexts_vseip(&self) -> AxResult<()> {
        for context_id in (1..self.contexts_num).step_by(2) {
            self.sync_vseip(context_id)?;
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
    /// Read operations are forwarded to the host PLIC for most registers,
    /// except for pending and claim/complete registers which are emulated.
    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> ax_errno::AxResult<usize> {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        // info!("vPlicGlobal read reg {reg:#x} width {width:?}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => perform_mmio_read(host_addr, width),
            // pending
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_PENDING_WORDS {
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
            PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => perform_mmio_read(host_addr, width),
            // threshold
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET).is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                perform_mmio_read(host_addr, width)
            }
            // claim/complete
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                let context_id =
                    (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                let Some(irq_id) = self.claim_next_irq(context_id)? else {
                    self.sync_vseip(context_id)?;
                    return Ok(0);
                };
                self.sync_vseip(context_id)?;
                Ok(irq_id)
            }
            _ => {
                unimplemented!("Unsupported vPlicGlobal read for reg {reg:#x}")
            }
        }
    }

    /// Handles MMIO write operations to the virtual PLIC.
    ///
    /// Only 32-bit (Dword) accesses are supported.
    /// Write operations are forwarded to the host PLIC for most registers.
    /// Writes to the pending register are used for interrupt injection by the hypervisor.
    /// Writes to the claim/complete register complete interrupt handling.
    fn handle_write(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
        val: usize,
    ) -> ax_errno::AxResult {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        // info!("vPlicGlobal write reg {reg:#x} width {width:?} val {val:#x}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                perform_mmio_write(host_addr, width, val)?;
                self.sync_all_guest_contexts_vseip()
            }
            // pending (Here is uesd for hyperivosr to inject pending IRQs, later should move it to a separate interface)
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                // Note: here append, not overwrite.
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_PENDING_WORDS {
                    return Ok(());
                }
                let val = val as u32;
                let mut bit_mask: u32 = 1;
                let mut pending_irqs = self.pending_irqs.lock();
                for i in 0..32 {
                    if (val & bit_mask) != 0 {
                        let irq_id = reg_index * 32 + i;
                        if irq_id != 0 {
                            // Set the pending bit.
                            pending_irqs.set(irq_id, true);
                        }
                    }
                    bit_mask <<= 1;
                }

                drop(pending_irqs);
                self.sync_all_guest_contexts_vseip()
            }
            // enable
            PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                perform_mmio_write(host_addr, width, val)?;
                let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                // A mask update can instantly expose or hide already-pending IRQs.
                self.sync_vseip(context_id)
            }
            // threshold
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET).is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                let context_id = (offset - PLIC_CONTEXT_CTRL_OFFSET) / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                perform_mmio_write(host_addr, width, val)?;
                // Threshold changes must be reflected on the hart line immediately.
                self.sync_vseip(context_id)
            }
            // claim/complete
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        .is_multiple_of(PLIC_CONTEXT_STRIDE) =>
            {
                // info!("vPlicGlobal: Writing to CLAIM/COMPLETE reg {reg:#x} val {val:#x}");
                let context_id =
                    (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                let irq_id = val;

                // The PLIC ignores completion IDs that do not correspond to an
                // enabled source for this context.
                if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                    return self.sync_vseip(context_id);
                }
                if !self
                    .context_enable_mask(context_id, irq_id / 32)
                    .map(|mask| (mask & (1 << (irq_id % 32))) != 0)?
                {
                    return self.sync_vseip(context_id);
                }
                let mut active_irqs = self.active_irqs.lock();
                if !active_irqs.get(irq_id) {
                    return self.sync_vseip(context_id);
                }

                // Write host PLIC.
                perform_mmio_write(host_addr, width, irq_id)?;
                // Clear the active bit only after the completion is accepted.
                active_irqs.set(irq_id, false);
                drop(active_irqs);
                self.sync_vseip(context_id)
            }
            _ => {
                unimplemented!("Unsupported vPlicGlobal read for reg {reg:#x}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axaddrspace::GuestPhysAddr;

    use super::*;

    #[test]
    fn pending_inactive_irqs_excludes_reserved_irq_zero() {
        let vplic = VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x400000), 2);

        {
            let mut pending_irqs = vplic.pending_irqs.lock();
            pending_irqs.set(0, true);
            pending_irqs.set(1, true);
        }

        let candidates = vplic.pending_inactive_irqs();

        assert!(!candidates.get(0));
        assert!(candidates.get(1));
    }
}
