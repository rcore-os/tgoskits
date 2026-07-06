//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddrRange, HostPhysAddr, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType, InterruptLineLevel, VcpuInterrupt};
use bitmaps::Bitmap;

use crate::{consts::*, utils::*, vplic::VPlicGlobal};

const SUPERVISOR_EXTERNAL_INTERRUPT: usize = 9;
const VIRTIO_MMIO_IRQ: usize = 8;
const VIRTIO_MMIO_BASE: usize = 0x1000_8000;
const VIRTIO_MMIO_INTERRUPT_STATUS: usize = VIRTIO_MMIO_BASE + 0x60;

impl VPlicGlobal {
    /// Reads the guest-visible priority of an interrupt source.
    fn irq_priority(&self, irq_id: usize) -> AxResult<u32> {
        Ok(self.priorities.lock()[irq_id])
    }

    /// Reads the priority threshold configured for a PLIC context.
    fn context_threshold(&self, context_id: usize) -> AxResult<u32> {
        Ok(self.thresholds.lock()[context_id])
    }

    /// Reads one enable register word for a PLIC context.
    fn context_enable_mask(&self, context_id: usize, reg_index: usize) -> AxResult<u32> {
        Ok(self.enable_masks.lock()[context_id][reg_index])
    }

    /// Updates the guest-visible priority for one interrupt source.
    fn set_irq_priority(&self, irq_id: usize, priority: u32) -> AxResult {
        self.priorities.lock()[irq_id] = priority;
        let host_addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize() + PLIC_PRIORITY_OFFSET + irq_id * 4,
        );
        perform_mmio_write(host_addr, AccessWidth::Dword, priority as usize)
    }

    /// Updates one guest-visible enable register word.
    fn set_context_enable_mask(&self, context_id: usize, reg_index: usize, val: u32) -> AxResult {
        let mut enable_masks = self.enable_masks.lock();
        let mut val = val;
        if reg_index == 0 {
            val &= !1;
        }
        enable_masks[context_id][reg_index] = val;

        let host_addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_ENABLE_OFFSET
                + context_id * PLIC_ENABLE_STRIDE
                + reg_index * 4,
        );
        let host_val = perform_mmio_read(host_addr, AccessWidth::Dword)? as u32;
        // Keep host PLIC enables permissive for passthrough sources. Guest
        // writes still update the virtual enable mask exactly, but a guest
        // disable/probe write should not mask the physical IRQ line that the
        // hypervisor depends on to inject the interrupt later.
        perform_mmio_write(host_addr, AccessWidth::Dword, (host_val | val) as usize)
    }

    /// Updates the guest-visible priority threshold for one context.
    fn set_context_threshold(&self, context_id: usize, threshold: u32) -> AxResult {
        self.thresholds.lock()[context_id] = threshold;
        let host_addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_CONTEXT_CTRL_OFFSET
                + context_id * PLIC_CONTEXT_STRIDE
                + PLIC_CONTEXT_THRESHOLD_OFFSET,
        );
        perform_mmio_write(host_addr, AccessWidth::Dword, threshold as usize)
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
            && priority > threshold
        {
            return Ok(Some(irq_id));
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

    /// Queues one IRQ if it is not already pending or in service.
    fn queue_irq_if_inactive(&self, irq_id: usize) -> bool {
        let mut pending_irqs = self.pending_irqs.lock();
        let active_irqs = self.active_irqs.lock();
        if pending_irqs.get(irq_id) || active_irqs.get(irq_id) {
            return false;
        }
        pending_irqs.set(irq_id, true);
        true
    }

    /// Recomputes whether VSEIP should remain asserted for one context.
    fn sync_vseip(&self, context_id: usize) -> AxResult<()> {
        let Some(vcpu_id) = guest_supervisor_context_vcpu_id(context_id) else {
            return Ok(());
        };
        // VSEIP should track whether this context still has a deliverable
        // external interrupt, not merely whether some pending bit is set.
        let level = if self.next_deliverable_irq(context_id)?.is_some() {
            InterruptLineLevel::Assert
        } else {
            InterruptLineLevel::Deassert
        };

        self.interrupt_sink.set_vcpu_interrupt(
            VcpuInterrupt {
                vcpu_id,
                vector: SUPERVISOR_EXTERNAL_INTERRUPT,
            },
            level,
        )
    }

    /// Recomputes VSEIP for all guest supervisor contexts.
    fn sync_all_guest_contexts_vseip(&self) -> AxResult<()> {
        for context_id in (1..self.contexts_num).step_by(2) {
            self.sync_vseip(context_id)?;
        }
        Ok(())
    }

    /// Claims host PLIC sources for passthrough devices and queues them in the
    /// virtual PLIC. Static-mode hosts may not have normal IRQ-line mappings
    /// for devices owned by the guest, so the hypervisor polls the host PLIC
    /// claim registers while the guest is running.
    pub fn poll_host_irqs(&self) -> AxResult<bool> {
        let mut claimed_any = false;

        for context_id in (1..self.contexts_num).step_by(2) {
            let host_claim_addr = HostPhysAddr::from_usize(
                self.host_plic_addr.as_usize()
                    + PLIC_CONTEXT_CTRL_OFFSET
                    + context_id * PLIC_CONTEXT_STRIDE
                    + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET,
            );
            let irq_id = perform_mmio_read(host_claim_addr, AccessWidth::Dword)?;
            if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                continue;
            }

            if self.queue_irq_if_inactive(irq_id) {
                claimed_any = true;
            }
        }

        let virtio_isr = perform_mmio_read(
            HostPhysAddr::from_usize(VIRTIO_MMIO_INTERRUPT_STATUS),
            AccessWidth::Dword,
        )?;
        if !claimed_any && virtio_isr != 0 {
            claimed_any = self.queue_irq_if_inactive(VIRTIO_MMIO_IRQ);
        }

        if claimed_any {
            self.sync_all_guest_contexts_vseip()?;
        }

        Ok(claimed_any)
    }
}

#[inline]
fn guest_supervisor_context_vcpu_id(context_id: usize) -> Option<usize> {
    // The guest PLIC exposes M-mode and S-mode contexts per hart. Linux uses
    // the odd S-mode contexts: hart0/context1, hart1/context3, ...
    (context_id % 2 == 1).then_some(context_id / 2)
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
    /// Only 32-bit (Dword) accesses are supported. Guest-visible PLIC state is
    /// emulated so Linux can freely probe contexts without changing host PLIC
    /// routing for passthrough sources.
    fn handle_read(
        &self,
        addr: <GuestPhysAddrRange as axaddrspace::device::DeviceAddrRange>::Addr,
        width: axaddrspace::device::AccessWidth,
    ) -> ax_errno::AxResult<usize> {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        // info!("vPlicGlobal read reg {reg:#x} width {width:?}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                let irq_id = (reg - PLIC_PRIORITY_OFFSET) / 4;
                Ok(self.irq_priority(irq_id)? as usize)
            }
            // pending
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_SOURCE_WORDS {
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
                let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                let word = ((reg - PLIC_ENABLE_OFFSET) % PLIC_ENABLE_STRIDE) / 4;
                if context_id >= self.contexts_num || word >= PLIC_SOURCE_WORDS {
                    return Ok(0);
                }
                Ok(self.context_enable_mask(context_id, word)? as usize)
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
                Ok(self.context_threshold(context_id)? as usize)
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
    /// Guest-visible configuration writes update virtual state. The host PLIC
    /// is kept permissive for passthrough sources so guest disable/probe writes
    /// cannot suppress real host external interrupts.
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
        // info!("vPlicGlobal write reg {reg:#x} width {width:?} val {val:#x}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => {
                let irq_id = (reg - PLIC_PRIORITY_OFFSET) / 4;
                self.set_irq_priority(irq_id, val as u32)?;
                self.sync_all_guest_contexts_vseip()
            }
            // pending (Here is uesd for hyperivosr to inject pending IRQs, later should move it to a separate interface)
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                // Note: here append, not overwrite.
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                if reg_index >= PLIC_SOURCE_WORDS {
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
                let context_id = (reg - PLIC_ENABLE_OFFSET) / PLIC_ENABLE_STRIDE;
                let word = ((reg - PLIC_ENABLE_OFFSET) % PLIC_ENABLE_STRIDE) / 4;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                assert!(word < PLIC_SOURCE_WORDS, "Invalid enable word {word}");
                self.set_context_enable_mask(context_id, word, val as u32)?;
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
                self.set_context_threshold(context_id, val as u32)?;
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

                if irq_id == 0 || irq_id >= PLIC_NUM_SOURCES {
                    return self.sync_vseip(context_id);
                }
                let mut active_irqs = self.active_irqs.lock();
                if !active_irqs.get(irq_id) {
                    return self.sync_vseip(context_id);
                }

                let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
                perform_mmio_write(host_addr, width, irq_id)?;
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
    use alloc::sync::Arc;

    use ax_errno::AxResult;
    use axaddrspace::GuestPhysAddr;
    use axdevice_base::{InterruptLineLevel, VcpuInterrupt, VmInterruptSink};

    use super::*;

    struct TestInterruptSink;

    impl VmInterruptSink for TestInterruptSink {
        fn set_vcpu_interrupt(
            &self,
            _interrupt: VcpuInterrupt,
            _level: InterruptLineLevel,
        ) -> AxResult {
            Ok(())
        }
    }

    fn test_interrupt_sink() -> Arc<dyn VmInterruptSink> {
        Arc::new(TestInterruptSink)
    }

    #[test]
    fn pending_inactive_irqs_excludes_reserved_irq_zero() {
        let vplic = VPlicGlobal::new(
            GuestPhysAddr::from(0x0c00_0000),
            Some(0x400000),
            2,
            test_interrupt_sink(),
        );

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
