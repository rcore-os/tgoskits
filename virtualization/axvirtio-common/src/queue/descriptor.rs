use alloc::vec::Vec;

use axvm_types::GuestPhysAddr;

use crate::{
    VirtioDeviceID,
    constants::*,
    error::{VirtioError, VirtioResult},
    memory::GuestMemory,
};

/// VirtIO queue descriptor structure.
///
/// This structure represents the memory layout of a single descriptor
/// in the descriptor table according to the VirtIO specification. It is
/// a C-compatible data structure that directly maps to guest memory.
///
/// Each descriptor describes a buffer in guest memory that can be used
/// for device I/O operations. Descriptors can be chained together using
/// the NEXT flag to describe scatter-gather buffers.
///
/// This structure is used by `DescriptorTable` to read/write individual
/// descriptors in guest memory through the guest memory accessor.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtQueueDesc {
    /// Address (guest-physical)
    pub base_addr: GuestPhysAddr,
    /// Length
    pub len: u32,
    /// Flags
    pub flags: u16,
    /// Next descriptor index (if VIRTQ_DESC_F_NEXT is set)
    pub next: u16,
}

impl VirtQueueDesc {
    /// Create a new descriptor
    pub fn new(base_addr: GuestPhysAddr, len: u32, flags: u16, next: u16) -> Self {
        Self {
            base_addr,
            len,
            flags,
            next,
        }
    }

    /// Check if this descriptor has the NEXT flag
    pub fn has_next(&self) -> bool {
        (self.flags & VIRTQ_DESC_F_NEXT) != 0
    }

    /// Check if this descriptor is writable
    pub fn is_write(&self) -> bool {
        (self.flags & VIRTQ_DESC_F_WRITE) != 0
    }

    /// Check if this descriptor is indirect
    pub fn is_indirect(&self) -> bool {
        (self.flags & VIRTQ_DESC_F_INDIRECT) != 0
    }

    /// Get the guest physical address
    pub fn guest_addr(&self) -> GuestPhysAddr {
        self.base_addr
    }

    /// Set the next flag
    pub fn set_next(&mut self, has_next: bool) {
        if has_next {
            self.flags |= VIRTQ_DESC_F_NEXT;
        } else {
            self.flags &= !VIRTQ_DESC_F_NEXT;
        }
    }

    /// Set the write flag
    pub fn set_write(&mut self, is_write: bool) {
        if is_write {
            self.flags |= VIRTQ_DESC_F_WRITE;
        } else {
            self.flags &= !VIRTQ_DESC_F_WRITE;
        }
    }

    /// Set the write flag (alias for compatibility)
    pub fn set_write_only(&mut self, is_write: bool) {
        self.set_write(is_write);
    }

    /// Check if this descriptor is write-only (alias for compatibility)
    pub fn is_write_only(&self) -> bool {
        self.is_write()
    }

    /// Set the indirect flag
    pub fn set_indirect(&mut self, is_indirect: bool) {
        if is_indirect {
            self.flags |= VIRTQ_DESC_F_INDIRECT;
        } else {
            self.flags &= !VIRTQ_DESC_F_INDIRECT;
        }
    }
}

/// A fully validated, device-agnostic VirtIO descriptor chain.
///
/// The common queue layer hands complete, direction-tagged chains to device
/// implementations (block, net, ...) so that each device can interpret its own
/// wire layout without leaking protocol specifics into the queue code.
///
/// Direction follows the VirtIO convention:
/// - `readable` descriptors are device-read (driver-written, not `WRITE`).
/// - `writable` descriptors are device-write (driver-read, `WRITE` set).
#[derive(Debug, Clone)]
pub struct DescriptorChain {
    /// Head descriptor index of this chain (the value read from the avail ring).
    head: u16,
    /// Descriptors in chain order, starting at `head`.
    descriptors: Vec<VirtQueueDesc>,
}

impl DescriptorChain {
    /// Construct a chain from its head index and ordered descriptors.
    pub fn new(head: u16, descriptors: Vec<VirtQueueDesc>) -> Self {
        Self { head, descriptors }
    }

    /// The head descriptor index (avail-ring entry value).
    pub fn head(&self) -> u16 {
        self.head
    }

    /// All descriptors in chain order.
    pub fn descriptors(&self) -> &[VirtQueueDesc] {
        &self.descriptors
    }

    /// Number of descriptors in the chain.
    pub fn len(&self) -> usize {
        self.descriptors.len()
    }

    /// Whether the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    /// Device-readable descriptors (driver-written, no `VIRTQ_DESC_F_WRITE`).
    pub fn readable(&self) -> impl Iterator<Item = &VirtQueueDesc> {
        self.descriptors.iter().filter(|d| !d.is_write())
    }

    /// Device-writable descriptors (`VIRTQ_DESC_F_WRITE` set).
    pub fn writable(&self) -> impl Iterator<Item = &VirtQueueDesc> {
        self.descriptors.iter().filter(|d| d.is_write())
    }

    /// Total bytes across device-readable descriptors, checked against overflow.
    pub fn readable_len(&self) -> VirtioResult<usize> {
        sum_descriptor_lens(self.readable().map(|d| d.len as usize))
    }

    /// Total bytes across device-writable descriptors, checked against overflow.
    pub fn writable_len(&self) -> VirtioResult<usize> {
        sum_descriptor_lens(self.writable().map(|d| d.len as usize))
    }
}

/// Checked sum of descriptor lengths; fails on overflow rather than wrapping.
fn sum_descriptor_lens(lens: impl Iterator<Item = usize>) -> VirtioResult<usize> {
    let mut total = 0usize;
    for v in lens {
        total = total.checked_add(v).ok_or(VirtioError::InvalidDescriptor)?;
    }
    Ok(total)
}

/// Descriptor table management structure.
///
/// This structure provides a high-level interface for managing the VirtIO
/// descriptor table in guest memory. It wraps the guest memory accessor and
/// provides methods to read/write individual descriptors and follow descriptor
/// chains.
///
/// Relationship with VirtQueueDesc:
/// - VirtQueueDesc defines the memory layout of a single descriptor
/// - DescriptorTable uses VirtQueueDesc to access descriptors in guest memory
/// - DescriptorTable manages the entire descriptor table and provides operations
///   for descriptor chains, validation, and buffer management
///
/// Memory Layout:
/// ```text
/// base_addr -> +-------------------+
///              | VirtQueueDesc[0]  |  (addr + len + flags + next)
///              +-------------------+
///              | VirtQueueDesc[1]  |  (addr + len + flags + next)
///              +-------------------+
///              | ...               |
///              +-------------------+
///              | VirtQueueDesc[n-1]|  (addr + len + flags + next)
///              +-------------------+
/// ```
///
/// Descriptor chains are formed by setting the NEXT flag and the next field
/// to link descriptors together, allowing scatter-gather I/O operations.
#[derive(Debug, Clone)]
pub struct DescriptorTable {
    /// Base address of the descriptor table
    pub base_addr: GuestPhysAddr,
    /// Number of descriptors
    pub size: u16,
}

impl DescriptorTable {
    /// Create a new descriptor table
    pub const fn new(base_addr: GuestPhysAddr, size: u16) -> Self {
        Self { base_addr, size }
    }

    /// Get the address of a specific descriptor
    pub fn desc_addr(&self, index: u16) -> Option<GuestPhysAddr> {
        if index >= self.size {
            return None;
        }

        let offset = index as usize * core::mem::size_of::<VirtQueueDesc>();
        Some(self.base_addr + offset)
    }

    /// Size in bytes of the descriptor table for a queue of `size` descriptors.
    ///
    /// Owns the `size * sizeof(VirtQueueDesc)` math so ring-region derivation
    /// (`VirtioQueue::ring_regions`) cannot drift from the per-descriptor
    /// address computation.
    pub(crate) const fn layout_size(size: u16) -> usize {
        size as usize * core::mem::size_of::<VirtQueueDesc>()
    }

    /// Calculate the total size of the descriptor table
    pub fn total_size(&self) -> usize {
        Self::layout_size(self.size)
    }

    /// Check if the descriptor table is valid
    pub fn is_valid(&self) -> bool {
        self.base_addr.as_usize() != 0 && self.size > 0
    }

    /// Read a descriptor from the table
    pub fn read_desc(
        &self,
        index: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<VirtQueueDesc> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let desc_addr = self.desc_addr(index).ok_or(VirtioError::InvalidQueue)?;

        let mut bytes = [0u8; 16];
        memory.read(desc_addr, &mut bytes)?;
        Ok(VirtQueueDesc {
            base_addr: GuestPhysAddr::from(
                u64::from_le_bytes(bytes[0..8].try_into().unwrap()) as usize
            ),
            len: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            flags: u16::from_le_bytes(bytes[12..14].try_into().unwrap()),
            next: u16::from_le_bytes(bytes[14..16].try_into().unwrap()),
        })
    }

    /// Write a descriptor to the table
    pub fn write_desc(
        &self,
        index: u16,
        desc: &VirtQueueDesc,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let desc_addr = self.desc_addr(index).ok_or(VirtioError::InvalidQueue)?;

        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&(desc.base_addr.as_usize() as u64).to_le_bytes());
        bytes[8..12].copy_from_slice(&desc.len.to_le_bytes());
        bytes[12..14].copy_from_slice(&desc.flags.to_le_bytes());
        bytes[14..16].copy_from_slice(&desc.next.to_le_bytes());
        memory.write(desc_addr, &bytes)?;

        Ok(())
    }

    /// Follow a descriptor chain starting from the given index
    pub fn follow_chain(
        &self,
        head_index: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<Vec<VirtQueueDesc>> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let mut descriptors = Vec::new();
        let mut current_index = head_index;

        loop {
            if current_index >= self.size {
                return Err(VirtioError::InvalidQueue);
            }

            let desc = self.read_desc(current_index, memory)?;
            descriptors.push(desc);

            if !desc.has_next() {
                break;
            }

            current_index = desc.next;

            // Prevent infinite loops
            if descriptors.len() > self.size as usize {
                return Err(VirtioError::InvalidQueue);
            }
        }

        Ok(descriptors)
    }

    /// Build a fully validated, device-agnostic [`DescriptorChain`] from a head
    /// index.
    ///
    /// Validation performed (all guest-provided input is untrusted):
    /// - `head` and every `next` index must be `< size`.
    /// - `VIRTQ_DESC_F_INDIRECT` is rejected (indirect descriptors are not
    ///   negotiated in the first version).
    /// - `base_addr + len` must not overflow.
    /// - The chain may reference at most `size` descriptors; a longer walk
    ///   indicates a cycle or a corrupted `next` field.
    pub fn descriptor_chain(
        &self,
        head: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<DescriptorChain> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }
        if head >= self.size {
            return Err(VirtioError::InvalidDescriptor);
        }

        let mut descriptors = Vec::new();
        let mut current = head;
        loop {
            if current >= self.size {
                return Err(VirtioError::InvalidDescriptor);
            }
            let desc = self.read_desc(current, memory)?;
            if desc.is_indirect() {
                return Err(VirtioError::NotSupported);
            }
            if desc
                .base_addr
                .as_usize()
                .checked_add(desc.len as usize)
                .is_none()
            {
                return Err(VirtioError::InvalidDescriptor);
            }
            descriptors.push(desc);
            if !desc.has_next() {
                break;
            }
            current = desc.next;
            // A chain referencing more than `size` descriptors is a cycle or
            // corruption. Bounding the walk also guarantees termination.
            if descriptors.len() > self.size as usize {
                return Err(VirtioError::InvalidDescriptor);
            }
        }

        Ok(DescriptorChain::new(head, descriptors))
    }

    /// Get the total length of a descriptor chain
    pub fn chain_length(&self, head_index: u16, memory: &mut dyn GuestMemory) -> VirtioResult<u32> {
        let descriptors = self.follow_chain(head_index, memory)?;
        Ok(descriptors.iter().map(|desc| desc.len).sum())
    }

    /// Check if a descriptor chain is valid
    pub fn validate_chain(
        &self,
        head_index: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<bool> {
        let descriptors = self.follow_chain(head_index, memory)?;

        // Basic validation: at least one descriptor
        if descriptors.is_empty() {
            return Ok(false);
        }

        // Check for proper flag usage
        for (i, desc) in descriptors.iter().enumerate() {
            // Last descriptor should not have NEXT flag
            if i == descriptors.len() - 1 && desc.has_next() {
                return Ok(false);
            }

            // Non-last descriptors should have NEXT flag
            if i < descriptors.len() - 1 && !desc.has_next() {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Get data buffer descriptors (excluding first and last)
    pub fn get_data_buffers(
        &self,
        head_index: u16,
        device_type: VirtioDeviceID,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<Vec<(GuestPhysAddr, usize, bool)>> {
        let descriptors = self.follow_chain(head_index, memory)?;

        if descriptors.len() < 2 && device_type == VirtioDeviceID::Block {
            return Ok(Vec::new());
        }

        let mut buffers = Vec::new();
        if device_type == VirtioDeviceID::Block {
            for desc in &descriptors[1..descriptors.len() - 1] {
                buffers.push((desc.base_addr, desc.len as usize, desc.is_write()));
            }
        } else {
            for desc in &descriptors {
                buffers.push((desc.base_addr, desc.len as usize, desc.is_write()));
            }
        }

        Ok(buffers)
    }

    /// Get the status descriptor address (last descriptor)
    pub fn get_status_addr(
        &self,
        head_index: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<GuestPhysAddr> {
        let descriptors = self.follow_chain(head_index, memory)?;

        if descriptors.is_empty() {
            return Err(VirtioError::InvalidQueue);
        }

        let status_desc = &descriptors[descriptors.len() - 1];
        // The status descriptor must be writable and at least 1 byte long
        if !status_desc.is_write() || status_desc.len < 1 {
            return Err(VirtioError::InvalidQueue);
        }

        Ok(status_desc.base_addr)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use ax_memory_addr::PhysAddr;
    use axaddrspace::GuestMemoryAccessor;

    use super::*;

    #[derive(Clone)]
    struct TestTranslator {
        base_host_ptr: usize,
    }

    impl GuestMemoryAccessor for TestTranslator {
        fn translate_and_get_limit(&self, guest_addr: GuestPhysAddr) -> Option<(PhysAddr, usize)> {
            let offset = guest_addr.as_usize();
            Some((PhysAddr::from(self.base_host_ptr + offset), usize::MAX))
        }
    }

    #[test]
    fn status_descriptor_len_must_be_at_least_one() {
        // Allocate a backing buffer to simulate host memory
        let mut mem = vec![0u8; 4096];
        let base_ptr = mem.as_mut_ptr() as usize;
        let translator = TestTranslator {
            base_host_ptr: base_ptr,
        };
        let mut memory = crate::AddressSpaceMemory::new(&translator);

        // Create a descriptor table at a non-zero guest base within our backing buffer
        let base = GuestPhysAddr::from(0x10usize);
        let table = DescriptorTable::new(base, 2);

        // Build a 2-descriptor chain: desc0 -> desc1
        let mut d0 = VirtQueueDesc::new(GuestPhysAddr::from(0x100usize), 16, 0, 1);
        d0.set_next(true);
        let mut d1 = VirtQueueDesc::new(GuestPhysAddr::from(0x200usize), 0, 0, 0);
        d1.set_write(true); // status descriptor must be write-only for device
        d1.set_next(false);

        table.write_desc(0, &d0, &mut memory).unwrap();
        table.write_desc(1, &d1, &mut memory).unwrap();

        // len == 0 should be invalid
        let err = table.get_status_addr(0, &mut memory).unwrap_err();
        assert!(matches!(err, VirtioError::InvalidQueue));

        // Fix len to 1, now it should pass
        let mut d1_ok = d1;
        d1_ok.len = 1;
        table.write_desc(1, &d1_ok, &mut memory).unwrap();
        let ok_addr = table.get_status_addr(0, &mut memory).unwrap();
        assert_eq!(ok_addr.as_usize(), 0x200);
    }

    #[test]
    fn layout_size_counts_descriptors() {
        // 4 descriptors * 16 bytes (VirtQueueDesc) = 64.
        assert_eq!(DescriptorTable::layout_size(4), 64);
        // Boundary: the largest queue size still counts every descriptor.
        assert_eq!(DescriptorTable::layout_size(256), 4096);
    }
}
