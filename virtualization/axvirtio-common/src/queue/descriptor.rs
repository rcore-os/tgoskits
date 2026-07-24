use crate::error::{VirtioError, VirtioResult};
use crate::{VirtioDeviceID, constants::*};
use alloc::sync::Arc;
use alloc::vec::Vec;
use axaddrspace::{GuestMemoryAccessor, GuestPhysAddr};

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
pub struct DescriptorTable<T: GuestMemoryAccessor + Clone> {
    /// Base address of the descriptor table
    pub base_addr: GuestPhysAddr,
    /// Number of descriptors
    pub size: u16,
    /// Guest memory accessor
    accessor: Arc<T>,
}

impl<T: GuestMemoryAccessor + Clone> DescriptorTable<T> {
    /// Create a new descriptor table
    pub fn new(base_addr: GuestPhysAddr, size: u16, accessor: Arc<T>) -> Self {
        Self {
            base_addr,
            size,
            accessor,
        }
    }

    /// Get the address of a specific descriptor
    pub fn desc_addr(&self, index: u16) -> Option<GuestPhysAddr> {
        if index >= self.size {
            return None;
        }

        let offset = index as usize * core::mem::size_of::<VirtQueueDesc>();
        Some(self.base_addr + offset)
    }

    /// Calculate the total size of the descriptor table
    pub fn total_size(&self) -> usize {
        self.size as usize * core::mem::size_of::<VirtQueueDesc>()
    }

    /// Check if the descriptor table is valid
    pub fn is_valid(&self) -> bool {
        self.base_addr.as_usize() != 0 && self.size > 0
    }

    /// Read a descriptor from the table
    pub fn read_desc(&self, index: u16) -> VirtioResult<VirtQueueDesc> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let desc_addr = self.desc_addr(index).ok_or(VirtioError::InvalidQueue)?;

        self.accessor
            .read_obj(desc_addr)
            .map_err(|_| VirtioError::InvalidAddress)
    }

    /// Write a descriptor to the table
    pub fn write_desc(&self, index: u16, desc: &VirtQueueDesc) -> VirtioResult<()> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let desc_addr = self.desc_addr(index).ok_or(VirtioError::InvalidQueue)?;

        self.accessor
            .write_obj(desc_addr, *desc)
            .map_err(|_| VirtioError::InvalidAddress)?;

        Ok(())
    }

    /// Follow a descriptor chain starting from the given index
    pub fn follow_chain(&self, head_index: u16) -> VirtioResult<Vec<VirtQueueDesc>> {
        if !self.is_valid() {
            return Err(VirtioError::QueueNotReady);
        }

        let mut descriptors = Vec::new();
        let mut current_index = head_index;

        loop {
            if current_index >= self.size {
                return Err(VirtioError::InvalidQueue);
            }

            let desc = self.read_desc(current_index)?;
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

    /// Get the total length of a descriptor chain
    pub fn chain_length(&self, head_index: u16) -> VirtioResult<u32> {
        let descriptors = self.follow_chain(head_index)?;
        Ok(descriptors.iter().map(|desc| desc.len).sum())
    }

    /// Check if a descriptor chain is valid
    pub fn validate_chain(&self, head_index: u16) -> VirtioResult<bool> {
        let descriptors = self.follow_chain(head_index)?;

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
    ) -> VirtioResult<Vec<(GuestPhysAddr, usize, bool)>> {
        let descriptors = self.follow_chain(head_index)?;

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
    pub fn get_status_addr(&self, head_index: u16) -> VirtioResult<GuestPhysAddr> {
        let descriptors = self.follow_chain(head_index)?;

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
    use super::*;
    use alloc::vec;
    use memory_addr::PhysAddr;

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
        let accessor = Arc::new(translator);

        // Create a descriptor table at a non-zero guest base within our backing buffer
        let base = GuestPhysAddr::from(0x10usize);
        let table: DescriptorTable<_> = DescriptorTable::new(base, 2, accessor.clone());

        // Build a 2-descriptor chain: desc0 -> desc1
        let mut d0 = VirtQueueDesc::new(GuestPhysAddr::from(0x100usize), 16, 0, 1);
        d0.set_next(true);
        let mut d1 = VirtQueueDesc::new(GuestPhysAddr::from(0x200usize), 0, 0, 0);
        d1.set_write(true); // status descriptor must be write-only for device
        d1.set_next(false);

        table.write_desc(0, &d0).unwrap();
        table.write_desc(1, &d1).unwrap();

        // len == 0 should be invalid
        let err = table.get_status_addr(0).unwrap_err();
        assert!(matches!(err, VirtioError::InvalidQueue));

        // Fix len to 1, now it should pass
        let mut d1_ok = d1;
        d1_ok.len = 1;
        table.write_desc(1, &d1_ok).unwrap();
        let ok_addr = table.get_status_addr(0).unwrap();
        assert_eq!(ok_addr.as_usize(), 0x200);
    }
}
