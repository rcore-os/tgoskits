//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use alloc::vec::Vec;
use core::option::Option;

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::Resource;
use axvm_types::GuestPhysAddr;
use bitmaps::Bitmap;

use crate::{VplicError, VplicResult, consts::*};

/// One guest-visible PLIC completion observed after controller state changed.
///
/// The event contains no host-controller state. Hypervisor adapters may use it
/// after all vPLIC locks are released to finish an optional physical backing
/// transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VplicCompletion {
    source: usize,
}

impl VplicCompletion {
    pub(crate) const fn new(source: usize) -> Self {
        Self { source }
    }

    /// Returns the completed guest PLIC source.
    pub const fn source(self) -> usize {
        self.source
    }
}

/// Virtual PLIC global controller.
///
/// Manages the state of a virtual PLIC device including interrupt assignment,
/// pending interrupts, and active interrupts for guest VMs.
pub struct VPlicGlobal {
    /// The address of the VPlicGlobal in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VPlicGlobal in bytes.
    pub size: usize,
    /// Stable guest resources declared to the V3 device runtime.
    pub(crate) resources: [Resource; 1],
    /// Num of contexts.
    pub contexts_num: usize,
    /// IRQs assigned to this VPlicGlobal.
    pub assigned_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Pending IRQs for this VPlicGlobal.
    pub pending_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Active IRQs for this VPlicGlobal.
    pub active_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Level-triggered inputs that remain electrically asserted.
    ///
    /// This is controller-owned state: completing a claimed source re-pends
    /// it until the device lowers the line.
    pub(crate) line_asserted_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Guest-programmable PLIC registers owned by this virtual controller.
    ///
    /// They must not alias host PLIC registers: guest configuration and
    /// claim/complete accesses belong to the VM, not the host interrupt domain.
    pub(crate) registers: Mutex<VPlicRegisters>,
}

/// Guest-visible PLIC priority, enable, and threshold registers.
pub(crate) struct VPlicRegisters {
    pub(crate) priorities: [u32; PLIC_NUM_SOURCES],
    pub(crate) enable_masks: Vec<[u32; PLIC_NUM_SOURCES / 32]>,
    pub(crate) thresholds: Vec<u32>,
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
            resources: [Resource::MmioRange {
                base: addr.as_usize() as u64,
                size: size as u64,
            }],
            assigned_irqs: Mutex::new(Bitmap::new()),
            pending_irqs: Mutex::new(Bitmap::new()),
            active_irqs: Mutex::new(Bitmap::new()),
            line_asserted_irqs: Mutex::new(Bitmap::new()),
            contexts_num,
            registers: Mutex::new(VPlicRegisters {
                priorities: [0; PLIC_NUM_SOURCES],
                enable_masks: alloc::vec![[0; PLIC_NUM_SOURCES / 32]; contexts_num],
                thresholds: alloc::vec![0; contexts_num],
            }),
        })
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
