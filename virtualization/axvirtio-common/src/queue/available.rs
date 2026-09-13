use alloc::sync::Arc;

use axaddrspace::GuestMemoryAccessor;
use axvm_types::GuestPhysAddr;

use crate::{
    constants::*,
    error::{VirtioError, VirtioResult},
    memory::GuestMemory,
};

/// VirtIO available ring header structure.
///
/// This structure represents the memory layout of the available ring header
/// in guest memory according to the VirtIO specification. It is a simple
/// C-compatible data structure that directly maps to guest memory.
///
/// The complete available ring in guest memory consists of:
/// 1. This header structure (VirtQueueAvail)
/// 2. An array of descriptor indices (ring[\queue_size])
/// 3. An optional used_event field (if VIRTIO_F_EVENT_IDX is negotiated)
///
/// This structure is used by `AvailableRing` to read/write the header portion
/// of the available ring through guest memory accessor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtQueueAvail {
    /// Flags
    pub flags: u16,
    /// Index of the next available descriptor
    pub idx: u16,
}

impl VirtQueueAvail {
    /// Create a new available ring header
    pub fn new() -> Self {
        Self { flags: 0, idx: 0 }
    }

    /// Check if interrupts are disabled
    pub fn no_interrupt(&self) -> bool {
        (self.flags & VIRTQ_AVAIL_F_NO_INTERRUPT) != 0
    }

    /// Set the no interrupt flag
    pub fn set_no_interrupt(&mut self, no_interrupt: bool) {
        if no_interrupt {
            self.flags |= VIRTQ_AVAIL_F_NO_INTERRUPT;
        } else {
            self.flags &= !VIRTQ_AVAIL_F_NO_INTERRUPT;
        }
    }
}

/// Available ring management structure.
///
/// This structure provides a high-level interface for managing the VirtIO
/// available ring in guest memory. It wraps the guest memory accessor and
/// provides methods to read/write various parts of the available ring:
/// - The header (VirtQueueAvail structure)
/// - The ring array of descriptor indices
/// - The used_event field (if VIRTIO_F_EVENT_IDX is negotiated)
///
/// Relationship with VirtQueueAvail:
/// - VirtQueueAvail defines the memory layout of the available ring header
/// - AvailableRing uses VirtQueueAvail to access the header in guest memory
/// - AvailableRing manages the entire available ring structure, not just the header
///
/// Memory Layout:
/// ```text
/// base_addr -> +-------------------+
///              | VirtQueueAvail    |  (flags + idx)
///              +-------------------+
///              | ring[0]           |  (descriptor index)
///              | ring[1]           |
///              | ...               |
///              | ring[queue_size-1]|
///              +-------------------+
///              | used_event        |  (optional, if event_idx enabled)
///              +-------------------+
/// ```
#[derive(Debug, Clone)]
pub struct AvailableRing<T: GuestMemoryAccessor + Clone> {
    /// Base address of the available ring
    pub base_addr: GuestPhysAddr,
    /// Queue size
    pub size: u16,
    /// Last seen available index
    pub last_avail_idx: u16,
    /// Guest memory accessor
    accessor: Arc<T>,
}

impl<T: GuestMemoryAccessor + Clone> AvailableRing<T> {
    /// Create a new available ring
    pub fn new(base_addr: GuestPhysAddr, size: u16, accessor: Arc<T>) -> Self {
        Self {
            base_addr,
            size,
            last_avail_idx: 0,
            accessor,
        }
    }

    /// Get the address of the available ring header
    pub fn header_addr(&self) -> GuestPhysAddr {
        self.base_addr
    }

    /// Get the address of the ring array
    pub fn ring_addr(&self) -> GuestPhysAddr {
        self.base_addr + core::mem::size_of::<VirtQueueAvail>()
    }

    /// Get the address of a specific ring entry
    pub fn ring_entry_addr(&self, index: u16) -> Option<GuestPhysAddr> {
        if index >= self.size {
            return None;
        }

        let offset = core::mem::size_of::<VirtQueueAvail>() + (index as usize * 2);
        Some(self.base_addr + offset)
    }

    /// Get the address of the used event field (if event_idx is enabled)
    pub fn used_event_addr(&self) -> GuestPhysAddr {
        // Header + ring array fill the region up to 2 bytes before its end;
        // the `used_event` footer is always part of the region (see
        // `layout_size`).
        self.base_addr + Self::layout_size(self.size) - 2
    }

    /// Size in bytes of the complete available ring for a queue of `size`
    /// entries, including the trailing 2-byte `used_event` field.
    ///
    /// The footer is always counted: a driver that negotiated
    /// `VIRTIO_F_RING_EVENT_IDX` writes `used_event` there, and layout
    /// validation must not depend on negotiation state.
    pub(crate) const fn layout_size(size: u16) -> usize {
        core::mem::size_of::<VirtQueueAvail>() + (size as usize) * 2 + 2
    }

    /// The size in bytes this ring occupies in guest memory, always including
    /// the trailing 2-byte event-index footer (see
    /// [`layout_size`](Self::layout_size)).
    pub fn total_size(&self) -> usize {
        Self::layout_size(self.size)
    }

    /// Check if the available ring is valid
    pub fn is_valid(&self) -> bool {
        self.base_addr.as_usize() != 0 && self.size > 0
    }

    /// Check if there are new available descriptors
    pub fn has_new_avail(&self, current_idx: u16) -> bool {
        current_idx != self.last_avail_idx
    }

    /// Update the last seen available index
    pub fn update_last_avail_idx(&mut self, idx: u16) {
        self.last_avail_idx = idx;
    }

    /// Read the available ring header
    pub fn read_avail_header(&self) -> VirtioResult<VirtQueueAvail> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        self.accessor
            .read_obj(self.base_addr)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Write the available ring header
    pub fn write_avail_header(&self, header: &VirtQueueAvail) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        self.accessor
            .write_obj(self.base_addr, header)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Read the current available index from guest memory
    pub fn read_avail_idx(&self) -> VirtioResult<u16> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        self.read_avail_idx_with_memory(&mut memory)
    }

    /// Reads the available index with a scoped memory capability.
    pub fn read_avail_idx_with_memory(&self, memory: &mut dyn GuestMemory) -> VirtioResult<u16> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        // Read the idx field from the header (offset 2 bytes for flags)
        let idx_addr = self.base_addr + 2;
        let mut bytes = [0u8; 2];
        memory.read(idx_addr, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Get the available index for external access
    pub fn get_avail_idx(&self) -> VirtioResult<u16> {
        self.read_avail_idx()
    }

    /// Read a descriptor index from the available ring
    pub fn read_avail_ring_entry(&self, ring_index: u16) -> VirtioResult<u16> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        self.read_avail_ring_entry_with_memory(ring_index, &mut memory)
    }

    /// Reads one available-ring entry with a scoped memory capability.
    pub fn read_avail_ring_entry_with_memory(
        &self,
        ring_index: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<u16> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let entry_addr = self
            .ring_entry_addr(ring_index % self.size)
            .ok_or(VirtioError::InvalidQueue)?;

        let mut bytes = [0u8; 2];
        memory.read(entry_addr, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Write a descriptor index to the available ring
    pub fn write_avail_ring_entry(&self, ring_index: u16, desc_index: u16) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let entry_addr = self
            .ring_entry_addr(ring_index % self.size)
            .ok_or(VirtioError::InvalidQueue)?;

        self.accessor
            .write_obj(entry_addr, desc_index)
            .map_err(|_| VirtioError::InvalidAddress)?;

        Ok(())
    }

    /// Get the number of available descriptors since last check
    pub fn get_available_count(&self) -> VirtioResult<u16> {
        let current_idx = self.read_avail_idx()?;
        Ok(current_idx.wrapping_sub(self.last_avail_idx))
    }

    /// Check if interrupts are suppressed
    pub fn interrupts_suppressed(&self) -> VirtioResult<bool> {
        let header = self.read_avail_header()?;
        Ok(header.no_interrupt())
    }

    /// Checks interrupt suppression with a scoped memory capability.
    pub fn interrupts_suppressed_with_memory(
        &self,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<bool> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        let mut bytes = [0u8; 2];
        memory.read(self.base_addr, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes) & VIRTQ_AVAIL_F_NO_INTERRUPT != 0)
    }

    /// Set interrupt suppression
    pub fn set_interrupt_suppression(&self, suppress: bool) -> VirtioResult<()> {
        let mut header = self.read_avail_header()?;
        header.set_no_interrupt(suppress);
        self.write_avail_header(&header)?;
        Ok(())
    }

    /// Read the used event field (for event_idx feature)
    pub fn read_used_event(&self) -> VirtioResult<u16> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let event_addr = self.used_event_addr();
        self.accessor
            .read_obj(event_addr)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Reads the used event field with a scoped memory capability.
    pub(crate) fn read_used_event_with_memory(
        &self,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<u16> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        let mut bytes = [0u8; 2];
        memory.read(self.used_event_addr(), &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Write the used event field (for event_idx feature)
    pub fn write_used_event(&self, event: u16) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let event_addr = self.used_event_addr();
        self.accessor
            .write_obj(event_addr, event)
            .map_err(|_| VirtioError::InvalidAddress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoGuestMemoryAccessor;

    #[test]
    fn layout_size_counts_header_entries_and_footer() {
        // header (4) + 4 entries * 2 bytes + used_event footer (2) = 14.
        assert_eq!(AvailableRing::<NoGuestMemoryAccessor>::layout_size(4), 14);
        // Boundary: the largest queue size still counts the footer.
        assert_eq!(
            AvailableRing::<NoGuestMemoryAccessor>::layout_size(256),
            4 + 256 * 2 + 2
        );
    }
}
