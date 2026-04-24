//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use core::option::Option;

use axaddrspace::{GuestPhysAddr, HostPhysAddr, device::AccessWidth};
use bitmaps::Bitmap;
use spin::Mutex;

use crate::{
    consts::*,
    utils::{perform_mmio_read, perform_mmio_write},
};

/// Virtual PLIC global controller.
///
/// Manages the state of a virtual PLIC device including interrupt assignment,
/// pending interrupts, and active interrupts for guest VMs.
pub struct VPlicGlobal {
    /// The address of the VPlicGlobal in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VPlicGlobal in bytes.
    pub size: usize,
    /// Num of contexts.
    pub contexts_num: usize,
    /// IRQs assigned to this VPlicGlobal.
    pub assigned_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Pending IRQs for this VPlicGlobal.
    pub pending_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Active IRQs for this VPlicGlobal.
    pub active_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// The host physical address of the PLIC.
    pub host_plic_addr: HostPhysAddr,
}

impl VPlicGlobal {
    pub fn bootstrap_host_passthrough_plic(&self) {
        // QEMU virt exposes 95 wired PLIC sources. For the current partial-passthrough
        // model, bootstrap the host PLIC so device IRQs can reach HS first, then let
        // the guest-side vPLIC decide claim/complete delivery.
        for irq_id in 1..=95 {
            let priority_addr = HostPhysAddr::from_usize(
                self.host_plic_addr.as_usize() + PLIC_PRIORITY_OFFSET + irq_id * 4,
            );
            perform_mmio_write(priority_addr, AccessWidth::Dword, 1)
                .expect("bootstrap host PLIC priority");
        }

        for context_id in (1..self.contexts_num).step_by(2) {
            let threshold_addr = HostPhysAddr::from_usize(
                self.host_plic_addr.as_usize()
                    + PLIC_CONTEXT_CTRL_OFFSET
                    + context_id * PLIC_CONTEXT_STRIDE
                    + PLIC_CONTEXT_THRESHOLD_OFFSET,
            );
            perform_mmio_write(threshold_addr, AccessWidth::Dword, 0)
                .expect("bootstrap host PLIC threshold");

            for reg_index in 0..=((95usize) / 32) {
                let mut val = u32::MAX;
                if reg_index == 0 {
                    val &= !1;
                }
                let enable_addr = HostPhysAddr::from_usize(
                    self.host_plic_addr.as_usize()
                        + PLIC_ENABLE_OFFSET
                        + context_id * PLIC_ENABLE_STRIDE
                        + reg_index * 4,
                );
                perform_mmio_write(enable_addr, AccessWidth::Dword, val as usize)
                    .expect("bootstrap host PLIC enable");
            }

            let claim_complete_addr = HostPhysAddr::from_usize(
                self.host_plic_addr.as_usize()
                    + PLIC_CONTEXT_CTRL_OFFSET
                    + context_id * PLIC_CONTEXT_STRIDE
                    + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET,
            );
            // Discard stale host-side completions from the period before the
            // guest takes ownership of the passthrough IRQ path.
            loop {
                let irq_id = perform_mmio_read(claim_complete_addr, AccessWidth::Dword)
                    .expect("bootstrap host PLIC claim");
                if irq_id == 0 {
                    break;
                }
                perform_mmio_write(claim_complete_addr, AccessWidth::Dword, irq_id)
                    .expect("bootstrap host PLIC complete");
            }
        }
    }

    /// Creates a new virtual PLIC global controller.
    ///
    /// # Arguments
    /// * `addr` - Guest physical address where the PLIC is mapped
    /// * `size` - Size of the PLIC memory region in bytes
    /// * `contexts_num` - Number of interrupt contexts (typically equal to number of harts)
    ///
    /// # Panics
    /// Panics if the provided size is insufficient to hold all PLIC registers.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>, contexts_num: usize) -> Self {
        let addr_end = addr.as_usize()
            + contexts_num * PLIC_CONTEXT_STRIDE
            + PLIC_CONTEXT_CTRL_OFFSET
            + PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET;
        let size = size.expect("Size must be specified for VPlicGlobal");
        assert!(
            addr.as_usize() + size > addr_end,
            "End address 0x{:x} exceeds region [0x{:x}, 0x{:x})  ",
            addr_end,
            addr.as_usize(),
            addr.as_usize() + size,
        );
        Self {
            addr,
            size,
            assigned_irqs: Mutex::new(Bitmap::new()),
            pending_irqs: Mutex::new(Bitmap::new()),
            active_irqs: Mutex::new(Bitmap::new()),
            contexts_num,
            // Current qemu-virt wiring assumes the guest-visible vPLIC aperture
            // overlays the same physical PLIC register block on the host.
            host_plic_addr: HostPhysAddr::from_usize(addr.as_usize()),
        }
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
