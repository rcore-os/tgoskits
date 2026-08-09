mod available;
mod descriptor;
mod used;

use alloc::{sync::Arc, vec::Vec};

pub use available::{AvailableRing, VirtQueueAvail};
use axaddrspace::GuestMemoryAccessor;
use axvm_types::GuestPhysAddr;
pub use descriptor::{DescriptorChain, DescriptorTable, VirtQueueDesc};
use log::trace;
pub use used::{UsedRing, VirtQueueUsed, VirtqUsedElem};

use crate::{
    VirtioDeviceID,
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

    /// Check if queue is valid and ready
    pub fn is_valid(&self) -> bool {
        self.ready
            && self.desc_table_addr.as_usize() != 0
            && self.avail_ring_addr.as_usize() != 0
            && self.used_ring_addr.as_usize() != 0
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
    pub fn read_avail_idx_with_memory(
        &self,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<u16> {
        self.avail_ring
            .as_ref()
            .ok_or(VirtioError::QueueNotReady)?
            .read_avail_idx_with_memory(memory)
    }

    /// Add a used buffer to the used ring
    pub fn add_used(&mut self, desc_index: u16, len: u32) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        // Use the UsedRing to properly manage the used ring
        if let Some(ref mut used_ring) = self.used_ring {
            used_ring.add_used(desc_index as u32, len)?;
            self.next_used = used_ring.get_used_idx();
        } else {
            // Fallback: just update the index
            self.next_used = (self.next_used + 1) % self.size;
        }

        Ok(())
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
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        let avail_idx = if let Some(avail_ring) = &self.avail_ring {
            avail_ring.read_avail_idx_with_memory(memory)?
        } else {
            return Err(VirtioError::QueueNotReady);
        };
        let last = self.get_last_avail_idx();
        let pending = avail_idx.wrapping_sub(last);
        if pending > self.size {
            return Err(VirtioError::InvalidQueue);
        }
        if pending == 0 {
            return Ok(None);
        }
        let head = if let Some(avail_ring) = &self.avail_ring {
            avail_ring.read_avail_ring_entry_with_memory(last % self.size, memory)?
        } else {
            return Err(VirtioError::QueueNotReady);
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
    pub fn descriptor_chain_with_memory(
        &self,
        head: u16,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<DescriptorChain> {
        if let Some(ref desc_table) = self.desc_table {
            desc_table.descriptor_chain(head, memory)
        } else {
            Err(VirtioError::QueueNotReady)
        }
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
    pub fn complete_with_memory(
        &mut self,
        head: u16,
        written_len: u32,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<bool> {
        if let Some(used_ring) = &mut self.used_ring {
            used_ring.add_used_with_memory(head as u32, written_len, memory)?;
            self.next_used = used_ring.get_used_idx();
        } else {
            return Err(VirtioError::QueueNotReady);
        }
        if let Some(avail_ring) = &self.avail_ring {
            Ok(!avail_ring.interrupts_suppressed_with_memory(memory)?)
        } else {
            Err(VirtioError::QueueNotReady)
        }
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
    pub fn read_avail_entry_with_memory(
        &self,
        ring_index: u16,
        memory: &mut dyn crate::GuestMemory,
    ) -> VirtioResult<u16> {
        self.avail_ring
            .as_ref()
            .ok_or(VirtioError::QueueNotReady)?
            .read_avail_ring_entry_with_memory(ring_index, memory)
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
    pub fn validate_virtio_block_chain(
        &self,
        head_index: u16,
        min_length: usize,
    ) -> VirtioResult<bool> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        if let Some(ref desc_table) = self.desc_table {
            let descriptors = desc_table.follow_chain(head_index, &mut memory)?;
            Ok(descriptors.len() >= min_length)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Get data buffers from descriptor chain
    pub fn get_data_buffers(
        &self,
        head_index: u16,
        device_type: VirtioDeviceID,
    ) -> VirtioResult<Vec<(GuestPhysAddr, usize, bool)>> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        if let Some(ref desc_table) = self.desc_table {
            desc_table.get_data_buffers(head_index, device_type, &mut memory)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Get status address from descriptor chain
    pub fn get_status_addr(&self, head_index: u16) -> VirtioResult<GuestPhysAddr> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        if let Some(ref desc_table) = self.desc_table {
            desc_table.get_status_addr(head_index, &mut memory)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Whether the device should interrupt the driver after updating the used ring.
    ///
    /// Per the VirtIO specification the device honors the *available* ring's
    /// `VIRTQ_AVAIL_F_NO_INTERRUPT` flag. The used ring's `VIRTQ_USED_F_NO_NOTIFY`
    /// flag is the opposite direction (the driver reads it to decide whether to
    /// kick the device), so it must not gate device-to-driver interrupts.
    pub fn should_notify(&self) -> VirtioResult<bool> {
        if let Some(ref avail_ring) = self.avail_ring {
            Ok(!avail_ring.interrupts_suppressed()?)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Write status byte to the status buffer of a descriptor chain
    ///
    /// This method writes the status byte to the last descriptor in the chain,
    /// which should be a write-only descriptor according to VirtIO specification.
    pub fn write_status_byte(&self, head_index: u16, status: u8) -> VirtioResult<()> {
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
