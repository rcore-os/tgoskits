//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use alloc::{sync::Arc, vec, vec::Vec};
use core::option::Option;

use ax_kspin::SpinNoIrq as Mutex;
use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use bitmaps::Bitmap;
use vm_interrupt::VmInterruptRouter;

use crate::consts::*;

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
    /// Guest-visible interrupt priorities.
    pub priorities: Mutex<[u32; PLIC_NUM_SOURCES]>,
    /// Guest-visible enable masks, indexed by context and source-word.
    pub enable_masks: Mutex<Vec<[u32; PLIC_SOURCE_WORDS]>>,
    /// Guest-visible priority thresholds, indexed by context.
    pub thresholds: Mutex<Vec<u32>>,
    /// Pending IRQs for this VPlicGlobal.
    pub pending_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Active IRQs for this VPlicGlobal.
    pub active_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// The host physical address of the PLIC.
    pub host_plic_addr: HostPhysAddr,
    /// Route table from guest PLIC context ID to VM-local vCPU ID.
    pub context_routes: Vec<Option<usize>>,
    /// VM-local interrupt routing endpoint for VSEIP updates.
    pub interrupt_router: Arc<dyn VmInterruptRouter>,
}

impl VPlicGlobal {
    /// Creates a new virtual PLIC global controller.
    ///
    /// # Arguments
    /// * `addr` - Guest physical address where the PLIC is mapped
    /// * `size` - Size of the PLIC memory region in bytes
    /// * `contexts_num` - Number of interrupt contexts (typically equal to number of harts)
    ///
    /// # Panics
    /// Panics if the provided size is insufficient to hold all PLIC registers.
    pub fn new(
        addr: GuestPhysAddr,
        size: Option<usize>,
        contexts_num: usize,
        context_routes: Vec<Option<usize>>,
        interrupt_router: Arc<dyn VmInterruptRouter>,
    ) -> Self {
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
        let mut context_routes = context_routes;
        context_routes.resize(contexts_num, None);

        Self {
            addr,
            size,
            assigned_irqs: Mutex::new(Bitmap::new()),
            priorities: Mutex::new([0; PLIC_NUM_SOURCES]),
            enable_masks: Mutex::new(vec![[0; PLIC_SOURCE_WORDS]; contexts_num]),
            thresholds: Mutex::new(vec![0; contexts_num]),
            pending_irqs: Mutex::new(Bitmap::new()),
            active_irqs: Mutex::new(Bitmap::new()),
            contexts_num,
            host_plic_addr: HostPhysAddr::from_usize(addr.as_usize()), /* Currently we assume host_plic_addr = guest_vplic_addr */
            context_routes,
            interrupt_router,
        }
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
