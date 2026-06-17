//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axbus::AtomicBitmap;

use crate::consts::*;

const BITMAP_WORDS: usize = PLIC_NUM_SOURCES / 64; // 16

/// Virtual PLIC global controller.
///
/// Manages the state of a virtual PLIC device including interrupt assignment,
/// pending interrupts, and active interrupts for guest VMs.
///
/// All bitmap fields use lock-free `AtomicBitmap` — safe to access from
/// any context including interrupt handlers.
pub struct VPlicGlobal {
    /// The address of the VPlicGlobal in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VPlicGlobal in bytes.
    pub size: usize,
    /// Num of contexts.
    pub contexts_num: usize,
    /// IRQs assigned to this VPlicGlobal.
    pub assigned_irqs: AtomicBitmap<BITMAP_WORDS>,
    /// Pending IRQs for this VPlicGlobal.
    pub pending_irqs: AtomicBitmap<BITMAP_WORDS>,
    /// Active IRQs for this VPlicGlobal.
    pub active_irqs: AtomicBitmap<BITMAP_WORDS>,
    /// The host physical address of the PLIC.
    pub host_plic_addr: HostPhysAddr,
}

impl VPlicGlobal {
    /// Creates a new virtual PLIC global controller.
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
            assigned_irqs: AtomicBitmap::new(),
            pending_irqs: AtomicBitmap::new(),
            active_irqs: AtomicBitmap::new(),
            contexts_num,
            host_plic_addr: HostPhysAddr::from_usize(addr.as_usize()),
        }
    }
}
