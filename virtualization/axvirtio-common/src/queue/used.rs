use alloc::sync::Arc;

use axaddrspace::GuestMemoryAccessor;
use axvm_types::GuestPhysAddr;
use mbarrier::mb;

use crate::{
    constants::*,
    error::{VirtioError, VirtioResult},
    memory::GuestMemory,
};

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
        // Header + ring array fill the region up to 2 bytes before its end;
        // the `avail_event` footer is always part of the region (see
        // `layout_size`).
        self.base_addr + Self::layout_size(self.size) - 2
    }

    /// Size in bytes of the complete used ring for a queue of `size` entries,
    /// including the trailing 2-byte `avail_event` field.
    ///
    /// The footer is always counted: a driver that negotiated
    /// `VIRTIO_F_RING_EVENT_IDX` reads `avail_event` from there, and layout
    /// validation must not depend on negotiation state.
    pub(crate) const fn layout_size(size: u16) -> usize {
        core::mem::size_of::<VirtQueueUsed>()
            + (size as usize) * core::mem::size_of::<VirtqUsedElem>()
            + 2
    }

    /// The size in bytes this ring occupies in guest memory, always including
    /// the trailing 2-byte event-index footer (see
    /// [`layout_size`](Self::layout_size)).
    pub fn total_size(&self) -> usize {
        Self::layout_size(self.size)
    }

    /// Check if the used ring is valid
    pub fn is_valid(&self) -> bool {
        self.base_addr.as_usize() != 0 && self.size > 0
    }

    /// Add a used element to the ring
    pub fn add_used(&mut self, id: u32, len: u32) -> VirtioResult<()> {
        let accessor = self.accessor.clone();
        let mut memory = crate::AddressSpaceMemory::new(&*accessor);
        self.add_used_with_memory(id, len, &mut memory)
    }

    /// Adds a used element with a scoped memory capability.
    pub fn add_used_with_memory(
        &mut self,
        id: u32,
        len: u32,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<()> {
        self.add_used_with_memory_and_barrier(id, len, memory, mb)
    }

    fn add_used_with_memory_and_barrier(
        &mut self,
        id: u32,
        len: u32,
        memory: &mut dyn GuestMemory,
        barrier: impl FnOnce(),
    ) -> VirtioResult<()> {
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
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&used_elem.id.to_le_bytes());
        bytes[4..8].copy_from_slice(&used_elem.len.to_le_bytes());
        memory.write(elem_addr, &bytes)?;

        // Update the used index
        self.used_idx = self.used_idx.wrapping_add(1);

        // Publish the used element before publishing used_idx to the driver.
        barrier();

        // Update the used ring header index
        self.write_used_idx_with_memory(memory)?;

        Ok(())
    }

    /// Write the used index to the used ring header
    pub fn write_used_idx(&self) -> VirtioResult<()> {
        let mut memory = crate::AddressSpaceMemory::new(&*self.accessor);
        self.write_used_idx_with_memory(&mut memory)
    }

    /// Writes the used index with a scoped memory capability.
    pub fn write_used_idx_with_memory(&self, memory: &mut dyn GuestMemory) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        // Write the used index to the header (offset 2 bytes for flags)
        let idx_addr = self.base_addr + 2;
        memory.write(idx_addr, &self.used_idx.to_le_bytes())?;

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

    /// Sets notification suppression with a scoped memory capability.
    pub(crate) fn set_notification_with_memory(
        &self,
        suppress: bool,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        let flags = if suppress { VIRTQ_USED_F_NO_NOTIFY } else { 0 };
        memory.write(self.base_addr, &flags.to_le_bytes())
    }

    /// Writes the available event field with a scoped memory capability.
    pub(crate) fn write_avail_event_with_memory(
        &self,
        event: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        memory.write(self.avail_event_addr(), &event.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec::Vec};
    use core::cell::RefCell;

    use axvm_types::GuestPhysAddr;

    use super::*;
    use crate::{GuestMemory, NoGuestMemoryAccessor};

    struct RecordingMemory {
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl GuestMemory for RecordingMemory {
        fn read(&mut self, _: GuestPhysAddr, _: &mut [u8]) -> VirtioResult<()> {
            Err(VirtioError::InvalidAddress)
        }

        fn write(&mut self, address: GuestPhysAddr, _: &[u8]) -> VirtioResult<()> {
            self.events
                .borrow_mut()
                .push(if address.as_usize() == 0x1002 {
                    "used_idx"
                } else {
                    "used_elem"
                });
            Ok(())
        }
    }

    #[test]
    fn publishes_used_element_before_used_index() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut memory = RecordingMemory {
            events: events.clone(),
        };
        let mut ring = UsedRing::new(
            GuestPhysAddr::from(0x1000),
            1,
            alloc::sync::Arc::new(NoGuestMemoryAccessor),
        );

        ring.add_used_with_memory_and_barrier(7, 11, &mut memory, || {
            events.borrow_mut().push("barrier");
        })
        .unwrap();

        assert_eq!(&*events.borrow(), &["used_elem", "barrier", "used_idx"]);
    }

    #[test]
    fn layout_size_counts_header_elements_and_footer() {
        // header (4) + 4 elements * 8 bytes + avail_event footer (2) = 38.
        assert_eq!(UsedRing::<NoGuestMemoryAccessor>::layout_size(4), 38);
        // Boundary: the largest queue size still counts the footer.
        assert_eq!(
            UsedRing::<NoGuestMemoryAccessor>::layout_size(256),
            4 + 256 * 8 + 2
        );
    }
}
