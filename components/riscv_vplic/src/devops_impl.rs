//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use ax_errno::AxResult;
use axaddrspace::{GuestPhysAddrRange, HostPhysAddr, device::AccessWidth};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

use crate::{consts::*, utils::*, vplic::VPlicGlobal};

const VCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS - 1);
const VCAUSE_VS_TIMER: usize = VCAUSE_INTERRUPT_BIT | 5;

impl VPlicGlobal {
    fn irq_priority(&self, irq_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize() + PLIC_PRIORITY_OFFSET + irq_id * 4,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    fn context_threshold(&self, context_id: usize) -> AxResult<u32> {
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_CONTEXT_CTRL_OFFSET
                + context_id * PLIC_CONTEXT_STRIDE
                + PLIC_CONTEXT_THRESHOLD_OFFSET,
        );
        Ok(perform_mmio_read(addr, AccessWidth::Dword)? as u32)
    }

    fn context_irq_enabled(&self, context_id: usize, irq_id: usize) -> AxResult<bool> {
        let reg_index = irq_id / 32;
        let bit_index = irq_id % 32;
        let addr = HostPhysAddr::from_usize(
            self.host_plic_addr.as_usize()
                + PLIC_ENABLE_OFFSET
                + context_id * PLIC_ENABLE_STRIDE
                + reg_index * 4,
        );
        let enabled_mask = perform_mmio_read(addr, AccessWidth::Dword)? as u32;
        Ok((enabled_mask & (1 << bit_index)) != 0)
    }

    fn next_claimable_irq(&self, context_id: usize) -> AxResult<Option<usize>> {
        let threshold = self.context_threshold(context_id)?;
        let pending_irqs = self.pending_irqs.lock();
        let active_irqs = self.active_irqs.lock();
        let mut best_irq = None;
        let mut best_priority = 0;

        // Follow the PLIC delivery rules instead of returning the first pending
        // bit: the IRQ must be pending, inactive, enabled for this context,
        // and above the context threshold.
        for irq_id in 1..PLIC_NUM_SOURCES {
            if !pending_irqs.get(irq_id) || active_irqs.get(irq_id) {
                continue;
            }
            if !self.context_irq_enabled(context_id, irq_id)? {
                continue;
            }

            let priority = self.irq_priority(irq_id)?;
            if priority <= threshold {
                continue;
            }
            if priority > best_priority {
                best_priority = priority;
                best_irq = Some(irq_id);
            }
        }

        Ok(best_irq)
    }

    fn sync_vseip(&self, context_id: usize) -> AxResult<()> {
        // VSEIP should track whether this context still has a deliverable
        // external interrupt, not merely whether some pending bit is set.
        if self.next_claimable_irq(context_id)?.is_some() {
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
                let bit_index_start = reg_index * 32;
                let mut val: u32 = 0;
                let mut bit_mask: u32 = 1;
                let pending_irqs = self.pending_irqs.lock();
                for i in 0..32 {
                    if pending_irqs.get(bit_index_start + i as usize) {
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
                let Some(irq_id) = self.next_claimable_irq(context_id)? else {
                    self.sync_vseip(context_id)?;
                    return Ok(0);
                };

                {
                    let mut pending_irqs = self.pending_irqs.lock();
                    // Claim moves the IRQ from pending to active until the guest
                    // writes it back to the complete register.
                    pending_irqs.set(irq_id, false);
                }
                self.active_irqs.lock().set(irq_id, true);
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
                for context_id in (1..self.contexts_num).step_by(2) {
                    self.sync_vseip(context_id)?;
                }
                Ok(())
            }
            // pending (Here is uesd for hyperivosr to inject pending IRQs, later should move it to a separate interface)
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                // Note: here append, not overwrite.
                let reg_index = (reg - PLIC_PENDING_OFFSET) / 4;
                let val = val as u32;
                let mut bit_mask: u32 = 1;
                let mut pending_irqs = self.pending_irqs.lock();
                for i in 0..32 {
                    if (val & bit_mask) != 0 {
                        let irq_id = reg_index * 32 + i;
                        // Set the pending bit.
                        pending_irqs.set(irq_id, true);
                    }
                    bit_mask <<= 1;
                }

                drop(pending_irqs);
                for context_id in (1..self.contexts_num).step_by(2) {
                    self.sync_vseip(context_id)?;
                }

                Ok(())
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

                // Clear the active bit, means the IRQ handling is complete.
                self.active_irqs.lock().set(irq_id, false);

                // Write host PLIC.
                perform_mmio_write(host_addr, width, irq_id)?;
                self.sync_vseip(context_id)
            }
            _ => {
                unimplemented!("Unsupported vPlicGlobal read for reg {reg:#x}")
            }
        }
    }
}
