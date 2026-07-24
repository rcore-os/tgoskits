use crate::constants::*;
use crate::error::{VirtioError, VirtioResult};
use alloc::sync::Arc;
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};

/// VirtIO used ring element structure.
///
/// This structure represents the memory layout of a single element in the
/// used ring array according to the VirtIO specification. Each element
/// records information about a completed descriptor chain.
///
/// This structure is used by `UsedRing` to read/write individual used
/// elements in guest memory through the guest memory accessor.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtqUsedElem {
    /// Index of start of used descriptor chain
    pub id: u32,
    /// Total length of the descriptor chain which was used
    pub len: u32,
}

impl VirtqUsedElem {
    /// Create a new used element
    pub fn new(id: u32, len: u32) -> Self {
        Self { id, len }
    }
}

/// VirtIO used ring header structure.
///
/// This structure represents the memory layout of the used ring header
/// in guest memory according to the VirtIO specification. It is a simple
/// C-compatible data structure that directly maps to guest memory.
///
/// The complete used ring in guest memory consists of:
/// 1. This header structure (VirtQueueUsed)
/// 2. An array of used elements (ring[\queue_size], each VirtqUsedElem)
/// 3. An optional avail_event field (if VIRTIO_F_EVENT_IDX is negotiated)
///
/// This structure is used by `UsedRing` to read/write the header portion
/// of the used ring through guest memory accessor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtQueueUsed {
    /// Flags
    pub flags: u16,
    /// Index of the next used element
    pub idx: u16,
    // Ring of used elements (variable length)
}

impl VirtQueueUsed {
    /// Create a new used ring header
    pub fn new() -> Self {
        Self { flags: 0, idx: 0 }
    }

    /// Check if notifications are disabled
    pub fn no_notify(&self) -> bool {
        (self.flags & VIRTQ_USED_F_NO_NOTIFY) != 0
    }

    /// Set the no notify flag
    pub fn set_no_notify(&mut self, no_notify: bool) {
        if no_notify {
            self.flags |= VIRTQ_USED_F_NO_NOTIFY;
        } else {
            self.flags &= !VIRTQ_USED_F_NO_NOTIFY;
        }
    }
}

/// Used ring management structure.
///
/// This structure provides a high-level interface for managing the VirtIO
/// used ring in guest memory. It wraps the guest memory accessor and
/// provides methods to read/write various parts of the used ring:
/// - The header (VirtQueueUsed structure)
/// - The ring array of used elements (VirtqUsedElem structures)
/// - The avail_event field (if VIRTIO_F_EVENT_IDX is negotiated)
///
/// Relationship with VirtQueueUsed and VirtqUsedElem:
/// - VirtQueueUsed defines the memory layout of the used ring header
/// - VirtqUsedElem defines the memory layout of each ring element
/// - UsedRing uses both structures to access the complete used ring in guest memory
/// - UsedRing manages the entire used ring structure and provides high-level operations
///
/// Memory Layout:
/// ```text
/// base_addr -> +-------------------+
///              | VirtQueueUsed     |  (flags + idx)
///              +-------------------+
///              | ring[0]           |  (VirtqUsedElem: id + len)
///              | ring[1]           |  (VirtqUsedElem: id + len)
///              | ...               |
///              | ring[queue_size-1]|  (VirtqUsedElem: id + len)
///              +-------------------+
///              | avail_event       |  (optional, if event_idx enabled)
///              +-------------------+
/// ```
#[derive(Debug, Clone)]
pub struct UsedRing<T: GuestMemoryAccessor + Clone> {
    /// Base address of the used ring
    pub base_addr: GuestPhysAddr,
    /// Queue size
    pub size: u16,
    /// Current used index
    pub used_idx: u16,
    /// Guest memory accessor
    accessor: Arc<T>,
}

impl<T: GuestMemoryAccessor + Clone> UsedRing<T> {
    /// Create a new used ring
    pub fn new(base_addr: GuestPhysAddr, size: u16, accessor: Arc<T>) -> Self {
        Self {
            base_addr,
            size,
            used_idx: 0,
            accessor,
        }
    }

    /// Get the address of the used ring header
    pub fn header_addr(&self) -> GuestPhysAddr {
        self.base_addr
    }

    /// Get the address of the ring array
    pub fn ring_addr(&self) -> GuestPhysAddr {
        self.base_addr + core::mem::size_of::<VirtQueueUsed>()
    }

    /// Get the address of a specific ring entry
    pub fn ring_entry_addr(&self, index: u16) -> Option<GuestPhysAddr> {
        if index >= self.size {
            return None;
        }

        let offset = core::mem::size_of::<VirtQueueUsed>()
            + (index as usize * core::mem::size_of::<VirtqUsedElem>());
        Some(self.base_addr + offset)
    }

    /// Get the address of the available event field (if event_idx is enabled)
    pub fn avail_event_addr(&self) -> GuestPhysAddr {
        let offset = core::mem::size_of::<VirtQueueUsed>()
            + (self.size as usize * core::mem::size_of::<VirtqUsedElem>());
        self.base_addr + offset
    }

    /// Calculate the total size of the used ring
    pub fn total_size(&self) -> usize {
        core::mem::size_of::<VirtQueueUsed>()
            + (self.size as usize * core::mem::size_of::<VirtqUsedElem>())
            + 2
    }

    /// Check if the used ring is valid
    pub fn is_valid(&self) -> bool {
        self.base_addr.as_usize() != 0 && self.size > 0
    }

    /// Add a used element to the ring
    pub fn add_used(&mut self, id: u32, len: u32) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        // Calculate the address of the used element to write
        let ring_index = self.used_idx % self.size;
        let elem_addr = self
            .ring_entry_addr(ring_index)
            .ok_or(VirtioError::InvalidQueue)?;

        // Create the used element
        let used_elem = VirtqUsedElem::new(id, len);

        // Write the used element to guest memory using injected memory accessor
        self.accessor
            .write_obj(elem_addr, used_elem)
            .map_err(|_| VirtioError::InvalidAddress)?;

        // Update the used index
        self.used_idx = self.used_idx.wrapping_add(1);

        // Update the used ring header index
        self.write_used_idx()?;

        Ok(())
    }

    /// Write the used index to the used ring header
    pub fn write_used_idx(&self) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        // Write the used index to the header (offset 2 bytes for flags)
        let idx_addr = self.base_addr + 2;
        self.accessor
            .write_obj(idx_addr, self.used_idx)
            .map_err(|_| VirtioError::InvalidAddress)?;

        Ok(())
    }

    /// Read the used ring header
    pub fn read_used_header(&self) -> VirtioResult<VirtQueueUsed> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        self.accessor
            .read_obj(self.base_addr)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Write the used ring header
    pub fn write_used_header(&self, header: &VirtQueueUsed) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        self.accessor
            .write_obj(self.base_addr, *header)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Get the current used index
    pub fn get_used_idx(&self) -> u16 {
        self.used_idx
    }

    /// Set the used index
    pub fn set_used_idx(&mut self, idx: u16) {
        self.used_idx = idx;
    }

    /// Check if notifications should be suppressed
    pub fn should_notify(&self) -> VirtioResult<bool> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let header = self.read_used_header()?;
        Ok(!header.no_notify())
    }

    /// Set notification suppression
    pub fn set_notification(&self, suppress: bool) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let mut header = self.read_used_header()?;
        header.set_no_notify(suppress);
        self.write_used_header(&header)?;

        Ok(())
    }
}
