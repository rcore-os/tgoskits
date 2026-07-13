//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use core::option::Option;

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::{GuestPhysAddr, HostPhysAddr};
use bitmaps::Bitmap;

use crate::{VplicError, VplicResult, consts::*};

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
    /// Creates a new virtual PLIC global controller.
    ///
    /// # Arguments
    /// * `addr` - Guest physical address where the PLIC is mapped
    /// * `size` - Size of the PLIC memory region in bytes
    /// * `contexts_num` - Number of interrupt contexts (typically equal to number of harts)
    ///
    /// # Errors
    ///
    /// Returns an error if `size` is absent, the address calculation
    /// overflows, or the region cannot cover all configured contexts.
    pub fn new(addr: GuestPhysAddr, size: Option<usize>, contexts_num: usize) -> VplicResult<Self> {
        let base = addr.as_usize();
        let required_end = contexts_num
            .checked_mul(PLIC_CONTEXT_STRIDE)
            .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CTRL_OFFSET))
            .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
            .and_then(|offset| base.checked_add(offset))
            .ok_or(VplicError::AddressOverflow)?;
        let size = size.ok_or(VplicError::MissingRegionSize)?;
        let region_end = base.checked_add(size).ok_or(VplicError::AddressOverflow)?;
        if region_end <= required_end {
            return Err(VplicError::InsufficientRegion {
                base,
                region_end,
                required_end,
            });
        }
        Ok(Self {
            addr,
            size,
            assigned_irqs: Mutex::new(Bitmap::new()),
            pending_irqs: Mutex::new(Bitmap::new()),
            active_irqs: Mutex::new(Bitmap::new()),
            contexts_num,
            host_plic_addr: HostPhysAddr::from_usize(addr.as_usize()), /* Currently we assume host_plic_addr = guest_vplic_addr */
        })
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
