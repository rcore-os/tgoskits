//! Virtual PLIC global controller.
//!
//! This module implements the core data structure for managing a virtual PLIC device.

use alloc::{boxed::Box, vec::Vec};
use core::{
    option::Option,
    sync::atomic::{AtomicU8, AtomicU32, Ordering},
};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::GuestPhysAddr;
use bitmaps::Bitmap;

use crate::{VplicError, VplicResult, consts::*};

const CONTEXT_LINE_ASSERTED: u8 = 1 << 0;
const CONTEXT_LINE_CHANGED: u8 = 1 << 1;

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
    /// Guest-owned priority registers, indexed by PLIC source ID.
    pub(crate) priorities: Box<[AtomicU32]>,
    /// Guest-owned enable words, laid out as context-major PLIC register words.
    pub(crate) context_enables: Box<[AtomicU32]>,
    /// Guest-owned priority threshold for every context.
    pub(crate) context_thresholds: Box<[AtomicU32]>,
    /// Physical sources whose completion ownership was transferred to the guest.
    pub(crate) forwarded_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Forwarded sources completed by the guest and awaiting host completion.
    pub(crate) completed_forwarded_irqs: Mutex<Bitmap<{ PLIC_NUM_SOURCES }>>,
    /// Software-owned interrupt line state for every PLIC context.
    context_lines: Box<[AtomicU8]>,
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
        let enable_words = contexts_num
            .checked_mul(PLIC_ENABLE_WORDS)
            .ok_or(VplicError::AddressOverflow)?;
        Ok(Self {
            addr,
            size,
            assigned_irqs: Mutex::new(Bitmap::new()),
            pending_irqs: Mutex::new(Bitmap::new()),
            active_irqs: Mutex::new(Bitmap::new()),
            priorities: (0..PLIC_NUM_SOURCES)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            context_enables: (0..enable_words)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            context_thresholds: (0..contexts_num)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            forwarded_irqs: Mutex::new(Bitmap::new()),
            completed_forwarded_irqs: Mutex::new(Bitmap::new()),
            context_lines: (0..contexts_num)
                .map(|_| AtomicU8::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            contexts_num,
        })
    }

    pub(crate) fn priority(&self, irq_id: usize) -> u32 {
        self.priorities[irq_id].load(Ordering::Acquire)
    }

    pub(crate) fn set_priority(&self, irq_id: usize, priority: u32) {
        if irq_id != 0 {
            self.priorities[irq_id].store(priority, Ordering::Release);
        }
    }

    fn context_enable_index(&self, context_id: usize, word: usize) -> VplicResult<usize> {
        self.validate_context_id(context_id)?;
        if word >= PLIC_ENABLE_WORDS {
            return Err(VplicError::InvalidEnableWord {
                word,
                words: PLIC_ENABLE_WORDS,
            });
        }
        Ok(context_id * PLIC_ENABLE_WORDS + word)
    }

    pub(crate) fn context_enable_word(&self, context_id: usize, word: usize) -> VplicResult<u32> {
        let index = self.context_enable_index(context_id, word)?;
        Ok(self.context_enables[index].load(Ordering::Acquire))
    }

    pub(crate) fn set_context_enable_word(
        &self,
        context_id: usize,
        word: usize,
        mut enabled: u32,
    ) -> VplicResult {
        let index = self.context_enable_index(context_id, word)?;
        if word == 0 {
            enabled &= !1;
        }
        self.context_enables[index].store(enabled, Ordering::Release);
        Ok(())
    }

    pub(crate) fn context_threshold_value(&self, context_id: usize) -> VplicResult<u32> {
        self.validate_context_id(context_id)?;
        Ok(self.context_thresholds[context_id].load(Ordering::Acquire))
    }

    pub(crate) fn set_context_threshold(&self, context_id: usize, threshold: u32) -> VplicResult {
        self.validate_context_id(context_id)?;
        self.context_thresholds[context_id].store(threshold, Ordering::Release);
        Ok(())
    }

    fn validate_context_id(&self, context_id: usize) -> VplicResult {
        if context_id >= self.contexts_num {
            return Err(VplicError::InvalidContext {
                context: context_id,
                contexts: self.contexts_num,
            });
        }
        Ok(())
    }

    pub(crate) fn update_context_line(&self, context_id: usize, asserted: bool) {
        let line = &self.context_lines[context_id];
        let asserted_bit = if asserted { CONTEXT_LINE_ASSERTED } else { 0 };
        let mut observed = line.load(Ordering::Acquire);
        loop {
            let mut updated = (observed & CONTEXT_LINE_CHANGED) | asserted_bit;
            if observed & CONTEXT_LINE_ASSERTED != asserted_bit {
                updated |= CONTEXT_LINE_CHANGED;
            }
            match line.compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(current) => observed = current,
            }
        }
    }

    /// Returns the current software interrupt-line level for one context.
    pub fn context_line_asserted(&self, context_id: usize) -> VplicResult<bool> {
        self.validate_context_id(context_id)?;
        Ok(self.context_lines[context_id].load(Ordering::Acquire) & CONTEXT_LINE_ASSERTED != 0)
    }

    /// Consumes a pending context-line transition and returns its latest level.
    ///
    /// The transition is device-owned software state. Consuming it never reads
    /// or writes a physical CPU CSR. If a producer changes the line again while
    /// this method runs, the new transition remains pending for a later call.
    pub fn take_context_notification(&self, context_id: usize) -> VplicResult<Option<bool>> {
        self.validate_context_id(context_id)?;
        let previous =
            self.context_lines[context_id].fetch_and(!CONTEXT_LINE_CHANGED, Ordering::AcqRel);
        if previous & CONTEXT_LINE_CHANGED == 0 {
            return Ok(None);
        }
        Ok(Some(previous & CONTEXT_LINE_ASSERTED != 0))
    }

    // pub fn assign_irq(&self, irq: u32, cpu_phys_id: usize, target_cpu_affinity: (u8, u8, u8, u8)) {
    //     warn!(
    //         "Assigning IRQ {} to vGICD at addr {:#x} for CPU phys id {} is not supported yet",
    //         irq, self.addr, cpu_phys_id
    //     );
    // }
}
