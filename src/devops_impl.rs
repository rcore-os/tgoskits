//! Device emulation operations for VPlicGlobal.
//!
//! Implements the `BaseDeviceOps` trait for MMIO read/write handling.

use crate::consts::*;
use crate::utils::*;
use crate::vplic::VPlicGlobal;
use axaddrspace::{device::AccessWidth, GuestPhysAddrRange, HostPhysAddr};
use axdevice_base::{BaseDeviceOps, EmuDeviceType};

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
    ) -> axerrno::AxResult<usize> {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        // info!("vPlicGlobal read reg {reg:#x} width {width:?}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => perform_mmio_read(host_addr, width),
            // pending
            PLIC_PENDING_OFFSET..PLIC_ENABLE_OFFSET => {
                let reg_index = reg - PLIC_PENDING_OFFSET / 4;
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
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET) % PLIC_CONTEXT_STRIDE == 0 =>
            {
                perform_mmio_read(host_addr, width)
            }
            // claim/complete
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        % PLIC_CONTEXT_STRIDE
                        == 0 =>
            {
                let context_id =
                    (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        / PLIC_CONTEXT_STRIDE;
                assert!(
                    context_id < self.contexts_num,
                    "Invalid context id {context_id}"
                );
                let mut pending_irqs = self.pending_irqs.lock();
                let irq_id = match pending_irqs.first_index() {
                    Some(id) => id,
                    None => return Ok(0),
                };

                // Check if the IRQ is belong to this context_id, check if is enabled, etc.
                // TODO: check enable bit and priority, threshold.

                // Clear the pending bit and set the active bit, means the IRQ is being handling.
                pending_irqs.set(irq_id, false);
                self.active_irqs.lock().set(irq_id, true);
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
    ) -> axerrno::AxResult {
        assert_eq!(width, AccessWidth::Dword);
        let reg = addr - self.addr;
        let host_addr = HostPhysAddr::from_usize(reg + self.host_plic_addr.as_usize());
        // info!("vPlicGlobal write reg {reg:#x} width {width:?} val {val:#x}");
        match reg {
            // priority
            PLIC_PRIORITY_OFFSET..PLIC_PENDING_OFFSET => perform_mmio_write(host_addr, width, val),
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
                        // info!("vPlicGlobal: IRQ {} set to pending", irq_id);
                    }
                    bit_mask <<= 1;
                }

                // Inject the interrupt to the hart by setting the VSEIP bit in HVIP register.
                if !pending_irqs.is_empty() {
                    unsafe {
                        riscv_h::register::hvip::set_vseip();
                    }
                }

                Ok(())
            }
            // enable
            PLIC_ENABLE_OFFSET..PLIC_CONTEXT_CTRL_OFFSET => {
                perform_mmio_write(host_addr, width, val)
            }
            // threshold
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET) % PLIC_CONTEXT_STRIDE == 0 =>
            {
                perform_mmio_write(host_addr, width, val)
            }
            // claim/complete
            offset
                if offset >= PLIC_CONTEXT_CTRL_OFFSET
                    && (offset - PLIC_CONTEXT_CTRL_OFFSET - PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET)
                        % PLIC_CONTEXT_STRIDE
                        == 0 =>
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

                // There is no irq to handle.
                if self.pending_irqs.lock().is_empty() {
                    unsafe {
                        riscv_h::register::hvip::clear_vseip();
                    }
                }

                // Clear the active bit, means the IRQ handling is complete.
                self.active_irqs.lock().set(irq_id, false);

                // Write host PLIC.
                perform_mmio_write(host_addr, width, irq_id)
            }
            _ => {
                unimplemented!("Unsupported vPlicGlobal read for reg {reg:#x}")
            }
        }
    }
}
