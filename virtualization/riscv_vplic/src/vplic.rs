//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use alloc::vec::Vec;
use core::{option::Option, sync::atomic::AtomicBool};

use ax_sync::Mutex;
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

/// Virtual PLIC global controller facade.
///
/// The facade is the task-facing `Send + Sync` device handle. Complete PLIC
/// state lives in [`VplicState`] and is changed by one owner transaction under
/// a sleepable mutex; no raw lock is embedded in the controller model.
pub struct VPlicGlobal {
    /// The address of the VPlicGlobal in the guest physical address space.
    pub addr: GuestPhysAddr,
    /// The size of the VPlicGlobal in bytes.
    pub size: usize,
    /// Stable guest resources declared to the V3 device runtime.
    pub(crate) resources: [Resource; 1],
    /// Num of contexts.
    pub contexts_num: usize,
    /// Mutable state owned by the vPLIC controller task.
    pub(crate) state: Mutex<VplicState>,
    /// Published VSEIP eligibility for each context. Entry code reads this
    /// snapshot without taking the controller's sleepable state lock.
    pub(crate) deliverable: Vec<AtomicBool>,
}

/// Complete guest-visible PLIC state owned by one controller task.
///
/// This type deliberately has no synchronization primitive. Keeping pending,
/// active, level and guest register state in one value makes claim/complete a
/// single transaction and prevents cross-lock observations of half a state
/// transition. The `VPlicGlobal` facade provides the shared task boundary.
pub struct VplicState {
    /// Sources assigned to this virtual controller.
    pub assigned_irqs: Bitmap<{ PLIC_NUM_SOURCES }>,
    /// Pending sources waiting to be claimed.
    pub pending_irqs: Bitmap<{ PLIC_NUM_SOURCES }>,
    /// Sources currently claimed by the guest.
    pub active_irqs: Bitmap<{ PLIC_NUM_SOURCES }>,
    /// Level-triggered sources that remain electrically asserted.
    pub line_asserted_irqs: Bitmap<{ PLIC_NUM_SOURCES }>,
    /// Guest-programmable priorities, enables and thresholds.
    pub registers: VPlicRegisters,
}

/// Guest-visible PLIC priority, enable, and threshold registers.
pub(crate) struct VPlicRegisters {
    pub priorities: [u32; PLIC_NUM_SOURCES],
    pub enable_masks: Vec<[u32; PLIC_NUM_SOURCES / 32]>,
    pub thresholds: Vec<u32>,
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
            contexts_num,
            state: Mutex::new(VplicState {
                assigned_irqs: Bitmap::new(),
                pending_irqs: Bitmap::new(),
                active_irqs: Bitmap::new(),
                line_asserted_irqs: Bitmap::new(),
                registers: VPlicRegisters {
                    priorities: [0; PLIC_NUM_SOURCES],
                    enable_masks: alloc::vec![[0; PLIC_NUM_SOURCES / 32]; contexts_num],
                    thresholds: alloc::vec![0; contexts_num],
                },
            }),
            deliverable: (0..contexts_num).map(|_| AtomicBool::new(false)).collect(),
        })
    }

    /// Runs a read-only query against the owner state.
    ///
    /// This is a task-context operation. Callers must use the vPLIC owner or
    /// its task-side facade; hard IRQ ingress publishes fixed atomic state and
    /// is drained by a worker before entering this method. Do not call it while
    /// holding a raw hardware guard, from a timer callback, or from a drop path.
    pub(crate) fn with_state<R>(&self, query: impl FnOnce(&VplicState) -> R) -> R {
        query(&self.state.lock())
    }

    /// Runs one complete owner transaction against the mutable state.
    ///
    /// The mutex is intentionally sleepable and may allocate in the supplied
    /// transaction. Keep this boundary in task context and publish any wake or
    /// IPI only after the transaction has returned and the lock is released.
    pub(crate) fn with_state_mut<R>(&self, update: impl FnOnce(&mut VplicState) -> R) -> R {
        let mut state = self.state.lock();
        let result = update(&mut state);
        self.refresh_deliverable(&state);
        result
    }

    /// Publishes the controller-derived VSEIP eligibility after a complete
    /// state transaction. The caller must hold the state lock while reading
    /// `state`; stores are released before an entry path consumes them.
    pub(crate) fn refresh_deliverable(&self, state: &VplicState) {
        for (context_id, deliverable_slot) in
            self.deliverable.iter().enumerate().take(self.contexts_num)
        {
            let deliverable = state
                .next_deliverable_irq(context_id)
                .ok()
                .flatten()
                .is_some();
            deliverable_slot.store(deliverable, core::sync::atomic::Ordering::Release);
        }
    }

    /// Reads the published VSEIP eligibility without entering the owner lock.
    pub fn context_deliverable(&self, context_id: usize) -> VplicResult<bool> {
        if context_id >= self.contexts_num {
            return Err(VplicError::InvalidContext {
                context: context_id,
                contexts: self.contexts_num,
            });
        }
        Ok(self.deliverable[context_id].load(core::sync::atomic::Ordering::Acquire))
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
