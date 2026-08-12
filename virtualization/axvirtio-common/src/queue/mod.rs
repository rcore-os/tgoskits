mod available;
mod descriptor;
mod used;

use alloc::{sync::Arc, vec::Vec};
use core::cell::Cell;

pub use available::{AvailableRing, VirtQueueAvail};
use axaddrspace::GuestMemoryAccessor;
use axvm_types::GuestPhysAddr;
pub use descriptor::{DescriptorChain, DescriptorTable, VirtQueueDesc};
use log::trace;
pub use used::{UsedRing, VirtQueueUsed, VirtqUsedElem};

use crate::{
    VirtioDeviceID,
    constants::{VIRTQ_AVAIL_ALIGN, VIRTQ_DESC_ALIGN, VIRTQ_USED_ALIGN},
    error::{VirtioError, VirtioResult},
};

/// VirtIO queue implementation
#[derive(Debug, Clone)]
pub struct VirtioQueue<T: GuestMemoryAccessor + Clone> {
    /// Queue index
    pub index: u16,
    /// Queue size
    pub size: u16,
    /// Descriptor table
    pub desc_table: Option<DescriptorTable>,
    /// Available ring
    avail_ring: Option<AvailableRing<T>>,
    /// Used ring
    used_ring: Option<UsedRing<T>>,
    /// Guest memory accessor
    accessor: Arc<T>,
    /// Maximum queue size
    pub max_size: u16,
    /// Queue ready flag
    pub ready: bool,
    /// Descriptor table address (guest physical)
    pub desc_table_addr: GuestPhysAddr,
    /// Available ring address (guest physical)
    pub avail_ring_addr: GuestPhysAddr,
    /// Used ring address (guest physical)
    pub used_ring_addr: GuestPhysAddr,
    /// Next available index
    next_avail: u16,
    /// Next used index
    next_used: u16,
    /// Event index enabled
    pub event_idx_enabled: bool,
    /// Set when a runtime ring/descriptor validation failure occurs; the queue
    /// rejects `pop`/`complete` and the guest-data paths until
    /// [`reset`](Self::reset) clears it.
    ///
    /// Uses interior mutability so that `&self` validation queries
    /// ([`descriptor_chain_with_memory`](Self::descriptor_chain_with_memory),
    /// [`get_status_addr`](Self::get_status_addr),
    /// [`should_notify`](Self::should_notify)) can latch the failure; the
    /// `&mut` paths (`pop`, `complete`, `reset`) only read the flag. The queue
    /// is always guarded by the owning device's lock (the MMIO transport holds
    /// its queue mutex for every access), so callers must never alias the
    /// queue across threads without that mutual exclusion; `Cell` is only
    /// sound under that single-owner rule.
    faulted: Cell<bool>,
}

impl<T: GuestMemoryAccessor + Clone> VirtioQueue<T> {
    /// Create a new VirtIO queue
    pub fn new(index: u16, size: u16, accessor: Arc<T>) -> Self {
        Self {
            index,
            size,
            desc_table: None,
            avail_ring: None,
            used_ring: None,
            accessor,
            max_size: size,
            ready: false,
            desc_table_addr: GuestPhysAddr::from(0),
            avail_ring_addr: GuestPhysAddr::from(0),
            used_ring_addr: GuestPhysAddr::from(0),
            next_avail: 0,
            next_used: 0,
            event_idx_enabled: false,
            faulted: Cell::new(false),
        }
    }

    /// Set queue size
    pub fn set_size(&mut self, size: u16) -> VirtioResult<()> {
        if size == 0 || size > self.max_size || (size & (size - 1)) != 0 {
            return Err(VirtioError::InvalidQueue);
        }
        self.size = size;
        Ok(())
    }

    /// Set descriptor table address
    pub fn set_desc_table_addr(&mut self, addr: GuestPhysAddr) -> VirtioResult<()> {
        // Overwrite semantics: VirtIO MMIO programs a 64-bit address via separate
        // LOW/HIGH 32-bit writes, so the setter accepts repeated updates and keeps
        // the latest combined value rather than rejecting the second write.
        self.desc_table_addr = addr;
        if addr.as_usize() != 0 {
            self.desc_table = Some(DescriptorTable::new(addr, self.size));
        } else {
            self.desc_table = None;
        }
        Ok(())
    }

    /// Set available ring address
    pub fn set_avail_ring_addr(&mut self, addr: GuestPhysAddr) -> VirtioResult<()> {
        self.avail_ring_addr = addr;
        if addr.as_usize() != 0 {
            self.avail_ring = Some(AvailableRing::new(addr, self.size, self.accessor.clone()));
        } else {
            self.avail_ring = None;
        }
        Ok(())
    }

    /// Set used ring address
    pub fn set_used_ring_addr(&mut self, addr: GuestPhysAddr) -> VirtioResult<()> {
        self.used_ring_addr = addr;
        if addr.as_usize() != 0 {
            self.used_ring = Some(UsedRing::new(addr, self.size, self.accessor.clone()));
        } else {
            self.used_ring = None;
        }
        Ok(())
    }

    /// Mark queue as ready
    pub fn set_ready(&mut self, ready: bool) {
        self.ready = ready;
    }

    /// Whether the three ring addresses have all been programmed.
    ///
    /// Address `0` is the "unconfigured" sentinel: a driver that has not
    /// finished programming a ring must never be able to make the queue ready.
    pub fn is_configured(&self) -> bool {
        self.desc_table_addr.as_usize() != 0
            && self.avail_ring_addr.as_usize() != 0
            && self.used_ring_addr.as_usize() != 0
    }

    /// The guest-memory accessor used by the non-`_with_memory` operations.
    pub fn accessor(&self) -> &Arc<T> {
        &self.accessor
    }

    /// Validate the three ring layouts against the VirtIO split-ring
    /// requirements. This is a pure query and does not change queue state.
    ///
    /// Checks, per VirtIO 1.x §2.7:
    /// - all three ring addresses are non-zero;
    /// - the descriptor table is 16-byte aligned, the available ring 2-byte and
    ///   the used ring 4-byte aligned;
    /// - `addr + size * elem_size` does not overflow the guest address space
    ///   for any ring;
    /// - the three regions do not overlap (overlap would let a used-element
    ///   write corrupt descriptors the device is about to read).
    ///
    /// The transport is expected to call this from its single "queue becomes
    /// usable" enforcement point (MMIO: the `QUEUE_READY` write; PCI: layout
    /// programmed in the queue config registers) and to refuse to mark the
    /// queue ready when it fails.
    pub fn validate_layout(&self) -> VirtioResult<()> {
        let regions = self.ring_regions();
        regions
            .iter()
            .all(|region| region.base.as_usize() != 0)
            .then_some(())
            .ok_or(VirtioError::InvalidRingLayout)?;
        if !self
            .desc_table_addr
            .as_usize()
            .is_multiple_of(VIRTQ_DESC_ALIGN)
            || !self
                .avail_ring_addr
                .as_usize()
                .is_multiple_of(VIRTQ_AVAIL_ALIGN)
            || !self
                .used_ring_addr
                .as_usize()
                .is_multiple_of(VIRTQ_USED_ALIGN)
        {
            return Err(VirtioError::RingMisaligned);
        }
        for (index, region) in regions.iter().enumerate() {
            if region.end().is_none() {
                return Err(VirtioError::InvalidRingLayout);
            }
            if regions[index + 1..]
                .iter()
                .any(|other| region.overlaps(other))
            {
                return Err(VirtioError::RingOverlap);
            }
        }
        Ok(())
    }

    /// Validate the ring layout like [`validate_layout`](Self::validate_layout)
    /// and additionally require every ring region to lie entirely inside the
    /// guest address space, translated through `memory`.
    ///
    /// `memory` must be the same capability used for the queue's runtime
    /// accesses, so a layout accepted here cannot fail later because a ring
    /// address is unmapped or reaches past the end of a mapped region. Any
    /// failure is reported as [`VirtioError::InvalidRingLayout`] so the caller
    /// only needs to distinguish "layout rejected" from "queue ready".
    pub fn validate_layout_with_memory(
        &self,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<()> {
        self.validate_layout()?;
        for region in self.ring_regions() {
            let end = region.end().ok_or(VirtioError::InvalidRingLayout)?;
            // `usize::MAX` denotes "no bound" for `GuestMemory` implementations
            // derived from an accessor without a real address space.
            if end == usize::MAX {
                continue;
            }
            if memory.read(region.base, &mut [0u8; 1]).is_err()
                || memory
                    .read(GuestPhysAddr::from(end - 1), &mut [0u8; 1])
                    .is_err()
            {
                return Err(VirtioError::InvalidRingLayout);
            }
        }
        Ok(())
    }

    /// The three ring regions derived from the current layout.
    fn ring_regions(&self) -> [RingRegion; 3] {
        let size = self.size as usize;
        let desc_size = size * core::mem::size_of::<VirtQueueDesc>();
        let avail_size = core::mem::size_of::<VirtQueueAvail>() + size * 2;
        let used_size =
            core::mem::size_of::<VirtQueueUsed>() + size * core::mem::size_of::<VirtqUsedElem>();
        [
            RingRegion::new(self.desc_table_addr, desc_size),
            RingRegion::new(self.avail_ring_addr, avail_size),
            RingRegion::new(self.used_ring_addr, used_size),
        ]
    }

    /// Whether the queue is in the faulted state and must be reset before
    /// further `pop`/`complete` calls.
    pub fn is_faulted(&self) -> bool {
        self.faulted.get()
    }

    /// Check if queue is valid and ready
    pub fn is_valid(&self) -> bool {
        self.ready
            && self.desc_table_addr.as_usize() != 0
            && self.avail_ring_addr.as_usize() != 0
            && self.used_ring_addr.as_usize() != 0
            && self.validate_layout().is_ok()
    }

    /// Latch the queue into the faulted state after a runtime validation
    /// failure; `pop`/`complete` are rejected until [`reset`](Self::reset).
    fn latch_fault(&self) {
        self.faulted.set(true);
    }

    /// Reset the queue
    pub fn reset(&mut self) {
        self.ready = false;
        self.desc_table_addr = GuestPhysAddr::from(0);
        self.avail_ring_addr = GuestPhysAddr::from(0);
        self.used_ring_addr = GuestPhysAddr::from(0);
        self.next_avail = 0;
        self.next_used = 0;
        self.desc_table = None;
        self.avail_ring = None;
        self.used_ring = None;
        self.faulted.set(false);
    }

    /// Read available ring index
    pub fn read_avail_idx(&self) -> VirtioResult<u16> {
        if let Some(ref avail_ring) = self.avail_ring {
            avail_ring.get_avail_idx()
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Reads the available index with a scoped memory capability.
    ///
    /// Returns [`VirtioError::QueueFaulted`] when the queue is faulted and
    /// [`VirtioError::QueueNotReady`] when the available ring is not
    /// configured (not a runtime failure, so the queue is not faulted). A read
    /// failure of a configured ring is a runtime failure and latches the
    /// fault, matching the other avail-ring pre-read paths.
    pub fn read_avail_idx_with_memory(
        &self,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<u16> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(avail_ring) = self.avail_ring.as_ref() else {
            return Err(VirtioError::QueueNotReady);
        };
        let result = avail_ring.read_avail_idx_with_memory(memory);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Add a used buffer to the used ring
    pub fn add_used(&mut self, desc_index: u16, len: u32) -> VirtioResult<()> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let result = if let Some(ref mut used_ring) = self.used_ring {
            let result = used_ring.add_used(desc_index as u32, len);
            if result.is_ok() {
                self.next_used = used_ring.get_used_idx();
            }
            result
        } else {
            // Fallback: just update the index
            self.next_used = (self.next_used + 1) % self.size;
            Ok(())
        };
        if result.is_err() {
            // A guest-memory write failure on a configured queue is a runtime
            // failure: latch the fault so no "error success" completion can
            // follow, mirroring `complete_with_memory`.
            self.latch_fault();
        }
        result
    }

    /// Consume one available-ring head index, or `None` if the queue is empty.
    ///
    /// Advances `last_avail_idx` by one (wrapping at `u16::MAX`). Returns
    /// [`VirtioError::InvalidQueue`] when the guest's `avail.idx` is ahead by
    /// more than `size`, which indicates a corrupted available ring.
    pub fn pop_available_head(&mut self) -> VirtioResult<Option<u16>> {
        let accessor = self.accessor.clone();
        let mut memory = crate::AddressSpaceMemory::new(&*accessor);
        self.pop_available_head_with_memory(&mut memory)
    }

    /// Consumes one available head with a scoped memory capability.
    pub fn pop_available_head_with_memory(
        &mut self,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<Option<u16>> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        let avail_idx = self.read_avail_idx_with_memory(memory)?;
        let last = self.get_last_avail_idx();
        let pending = avail_idx.wrapping_sub(last);
        if pending > self.size {
            // Corrupted available ring: more entries pending than the queue
            // can hold. Latch the fault so the queue stops serving until reset.
            self.latch_fault();
            return Err(VirtioError::InvalidQueue);
        }
        if pending == 0 {
            return Ok(None);
        }
        let head = match self
            .avail_ring
            .as_ref()
            .map(|ring| ring.read_avail_ring_entry_with_memory(last % self.size, memory))
        {
            Some(Ok(head)) => head,
            Some(Err(error)) => {
                // A guest-memory read failure on a configured queue is a
                // runtime failure; latch the fault like the other runtime
                // paths do.
                self.latch_fault();
                return Err(error);
            }
            None => return Err(VirtioError::QueueNotReady),
        };
        self.update_last_avail_idx(last.wrapping_add(1));
        Ok(Some(head))
    }

    /// Consume one available head and return a validated [`DescriptorChain`].
    ///
    /// Returns `Ok(None)` when the queue is empty. The head is consumed *before*
    /// the chain is validated; on a validation error the head is already
    /// advanced (so the queue is not stalled) and the caller should complete
    /// that head with length 0. To recover the head on error, use
    /// [`pop_available_head`](Self::pop_available_head) plus
    /// [`descriptor_chain`](Self::descriptor_chain) directly.
    pub fn pop_available(&mut self) -> VirtioResult<Option<DescriptorChain>> {
        let head = match self.pop_available_head()? {
            Some(h) => h,
            None => return Ok(None),
        };
        Ok(Some(self.descriptor_chain(head)?))
    }

    /// Build a validated [`DescriptorChain`] for an already-consumed head index.
    pub fn descriptor_chain(&self, head: u16) -> VirtioResult<DescriptorChain> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        self.descriptor_chain_with_memory(head, &mut memory)
    }

    /// Builds a validated descriptor chain using a scoped memory capability.
    ///
    /// Returns [`VirtioError::QueueNotReady`] when the descriptor table is not
    /// configured (not a runtime failure, so the queue is not faulted) and
    /// [`VirtioError::QueueFaulted`] when the queue is already faulted.
    pub fn descriptor_chain_with_memory(
        &self,
        head: u16,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<DescriptorChain> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(ref desc_table) = self.desc_table else {
            return Err(VirtioError::QueueNotReady);
        };
        let result = desc_table.descriptor_chain(head, memory);
        if result.is_err() {
            // The descriptor table is configured, so a chain failure is a
            // runtime validation failure: latch the fault.
            self.latch_fault();
        }
        result
    }

    /// Complete a descriptor chain: append a used element for `head` with the
    /// given written length, then report whether the driver should be notified.
    ///
    /// `written_len` is the number of bytes the device wrote into guest-writable
    /// buffers (RX bytes, or 0 for TX / discarded / error completions).
    pub fn complete(&mut self, head: u16, written_len: u32) -> VirtioResult<bool> {
        self.add_used(head, written_len)?;
        self.should_notify()
    }

    /// Completes a chain with a scoped memory capability.
    ///
    /// Returns [`VirtioError::QueueNotReady`] when a ring is not configured
    /// (not a runtime failure, so the queue is not faulted) and
    /// [`VirtioError::QueueFaulted`] when the queue is already faulted. Any
    /// failure while writing the used ring or reading the available flags is
    /// treated as a runtime failure and latches the fault.
    pub fn complete_with_memory(
        &mut self,
        head: u16,
        written_len: u32,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<bool> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let result = (|| {
            let used_ring = self.used_ring.as_mut().ok_or(VirtioError::QueueNotReady)?;
            used_ring.add_used_with_memory(head as u32, written_len, memory)?;
            self.next_used = used_ring.get_used_idx();
            let avail_ring = self.avail_ring.as_ref().ok_or(VirtioError::QueueNotReady)?;
            Ok(!avail_ring.interrupts_suppressed_with_memory(memory)?)
        })();
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Get the used ring reference
    pub fn get_used_ring(&self) -> Option<&UsedRing<T>> {
        self.used_ring.as_ref()
    }

    /// Get the used ring mutable reference
    pub fn get_used_ring_mut(&mut self) -> Option<&mut UsedRing<T>> {
        self.used_ring.as_mut()
    }

    /// Get the available ring reference
    pub fn get_avail_ring(&self) -> Option<&AvailableRing<T>> {
        self.avail_ring.as_ref()
    }

    /// Get the descriptor table reference
    pub fn get_desc_table(&self) -> Option<&DescriptorTable> {
        self.desc_table.as_ref()
    }

    /// Read available ring entry
    pub fn read_avail_entry(&self, ring_index: u16) -> VirtioResult<u16> {
        if let Some(ref avail_ring) = self.avail_ring {
            avail_ring.read_avail_ring_entry(ring_index)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Reads an available-ring entry with a scoped memory capability.
    ///
    /// Returns [`VirtioError::QueueFaulted`] when the queue is faulted and
    /// [`VirtioError::QueueNotReady`] when the available ring is not
    /// configured (not a runtime failure, so the queue is not faulted). A read
    /// failure of a configured ring is a runtime failure and latches the
    /// fault, matching the other avail-ring pre-read paths.
    pub fn read_avail_entry_with_memory(
        &self,
        ring_index: u16,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<u16> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(avail_ring) = self.avail_ring.as_ref() else {
            return Err(VirtioError::QueueNotReady);
        };
        let result = avail_ring.read_avail_ring_entry_with_memory(ring_index, memory);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Update last available index
    pub fn update_last_avail_idx(&mut self, idx: u16) {
        if let Some(ref mut avail_ring) = self.avail_ring {
            avail_ring.update_last_avail_idx(idx);
        } else {
            self.next_avail = idx % self.size;
        }
    }

    /// Get last available index
    pub fn get_last_avail_idx(&self) -> u16 {
        if let Some(avail_ring) = &self.avail_ring {
            avail_ring.last_avail_idx
        } else {
            self.next_avail
        }
    }

    /// Validate VirtIO block chain
    ///
    /// Returns [`VirtioError::QueueNotReady`] when the descriptor table is not
    /// configured and [`VirtioError::QueueFaulted`] when the queue is faulted.
    /// Any validation or guest-memory failure on a configured queue latches
    /// the fault, matching the other descriptor-chain walk entry points.
    pub fn validate_virtio_block_chain(
        &self,
        head_index: u16,
        min_length: usize,
    ) -> VirtioResult<bool> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(ref desc_table) = self.desc_table else {
            return Err(VirtioError::QueueNotReady);
        };
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        let result = desc_table
            .follow_chain(head_index, &mut memory)
            .map(|descriptors| descriptors.len() >= min_length);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Get data buffers from descriptor chain
    ///
    /// Returns [`VirtioError::QueueNotReady`] when the descriptor table is not
    /// configured and [`VirtioError::QueueFaulted`] when the queue is faulted.
    /// Any guest-memory failure on a configured queue latches the fault.
    pub fn get_data_buffers(
        &self,
        head_index: u16,
        device_type: VirtioDeviceID,
    ) -> VirtioResult<Vec<(GuestPhysAddr, usize, bool)>> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(ref desc_table) = self.desc_table else {
            return Err(VirtioError::QueueNotReady);
        };
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        let result = desc_table.get_data_buffers(head_index, device_type, &mut memory);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Get status address from descriptor chain
    ///
    /// Returns [`VirtioError::QueueNotReady`] when the descriptor table is not
    /// configured and [`VirtioError::QueueFaulted`] when the queue is faulted.
    /// Any guest-memory failure on a configured queue latches the fault.
    pub fn get_status_addr(&self, head_index: u16) -> VirtioResult<GuestPhysAddr> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(ref desc_table) = self.desc_table else {
            return Err(VirtioError::QueueNotReady);
        };
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        let result = desc_table.get_status_addr(head_index, &mut memory);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Whether the device should interrupt the driver after updating the used ring.
    ///
    /// Per the VirtIO specification the device honors the *available* ring's
    /// `VIRTQ_AVAIL_F_NO_INTERRUPT` flag. The used ring's `VIRTQ_USED_F_NO_NOTIFY`
    /// flag is the opposite direction (the driver reads it to decide whether to
    /// kick the device), so it must not gate device-to-driver interrupts.
    ///
    /// Returns [`VirtioError::QueueFaulted`] when the queue is faulted,
    /// [`VirtioError::QueueNotReady`] when the available ring is not
    /// configured (not a runtime failure, so the queue is not faulted), and
    /// latches the fault on a read failure of a configured ring.
    pub fn should_notify(&self) -> VirtioResult<bool> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        let Some(ref avail_ring) = self.avail_ring else {
            return Err(VirtioError::QueueNotReady);
        };
        let result = avail_ring
            .interrupts_suppressed()
            .map(|suppressed| !suppressed);
        if result.is_err() {
            self.latch_fault();
        }
        result
    }

    /// Write status byte to the status buffer of a descriptor chain
    ///
    /// This method writes the status byte to the last descriptor in the chain,
    /// which should be a write-only descriptor according to VirtIO specification.
    ///
    /// Returns [`VirtioError::QueueNotReady`] when the descriptor table is not
    /// configured and [`VirtioError::QueueFaulted`] when the queue is faulted;
    /// a faulted queue never writes guest memory.
    pub fn write_status_byte(&self, head_index: u16, status: u8) -> VirtioResult<()> {
        if self.faulted.get() {
            return Err(VirtioError::QueueFaulted);
        }
        // Get the status descriptor address (last descriptor in chain)
        let status_addr_guest = self.get_status_addr(head_index)?;

        trace!(
            "Writing status byte {} to guest address 0x{:x} for descriptor chain {}",
            status,
            status_addr_guest.as_usize(),
            head_index
        );

        // Write the status byte to guest memory using the new memory access interface
        self.accessor
            .write_obj(status_addr_guest, status)
            .map_err(|_| VirtioError::InvalidAddress)?;

        Ok(())
    }
}

/// One half-open guest region `[base, base + size)` used by ring-layout
/// validation. Deliberately uses `usize` arithmetic like the rest of the queue
/// layer so the checks match the arithmetic actually performed on ring
/// accesses.
#[derive(Clone, Copy)]
struct RingRegion {
    base: GuestPhysAddr,
    size: usize,
}

impl RingRegion {
    fn new(base: GuestPhysAddr, size: usize) -> Self {
        Self { base, size }
    }

    /// The exclusive end address, or `None` if `base + size` overflows the
    /// guest address space.
    fn end(&self) -> Option<usize> {
        self.base.as_usize().checked_add(self.size)
    }

    /// Whether the two regions share any byte.
    ///
    /// A region whose end overflows the address space is unbounded; treating
    /// it as non-overlapping would let a wrap-around ring alias the memory of
    /// a neighbouring ring, so an overflowing region always overlaps.
    fn overlaps(&self, other: &Self) -> bool {
        let Some(self_end) = self.end() else {
            return true;
        };
        let Some(other_end) = other.end() else {
            return true;
        };
        self.base.as_usize() < other_end && other.base.as_usize() < self_end
    }
}
