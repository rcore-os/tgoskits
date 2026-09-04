//! Transport-independent VirtIO block request processing.

use alloc::vec::Vec;
use core::ops::Range;

use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{GuestMemory, VirtioError, VirtioQueue, VirtioResult, queue::VirtQueueDesc};
use log::warn;

use crate::{
    BlockBackend, VirtioBlockConfig,
    block::VIRTIO_BLK_REQUEST_HEADER_SIZE,
    constants::{
        MIN_DESCRIPTOR_CHAIN_LENGTH, SECTOR_SIZE, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK,
        VIRTIO_BLK_S_UNSUPP, VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
    },
};

/// Result of servicing all currently available descriptors in one block queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockQueueOutcome {
    /// The queue contained no available request.
    Idle,
    /// At least one request completed.
    Completed {
        /// Whether the split-ring notification rules request an interrupt.
        notify: bool,
    },
    /// A request was retained because the backend cannot complete it yet.
    Deferred {
        /// Descriptor head that the caller must retain for the next poll.
        pending_head: u16,
        /// Whether an earlier completion requests an interrupt.
        notify: bool,
    },
}

/// Transport-independent VirtIO block request core.
///
/// The transport owns registers, feature negotiation, queue configuration and
/// interrupt delivery. This core owns backend policy and operates only on a
/// caller-provided queue and guest memory. Deferred request ownership remains
/// with the transport or runtime that invokes the core.
pub struct VirtioBlockRequestCore<B: BlockBackend> {
    config: VirtioBlockConfig,
    backend: B,
}

impl<B: BlockBackend> core::fmt::Debug for VirtioBlockRequestCore<B> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("VirtioBlockRequestCore")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<B: BlockBackend> VirtioBlockRequestCore<B> {
    /// Creates a block request core with immutable device policy.
    pub const fn new(backend: B, config: VirtioBlockConfig) -> Self {
        Self { config, backend }
    }

    /// Returns the block configuration exposed by the transport.
    pub const fn config(&self) -> &VirtioBlockConfig {
        &self.config
    }

    /// Returns whether backend work must run from a deferred runtime context.
    pub fn requires_deferred_processing(&self) -> bool {
        self.backend.requires_deferred_processing()
    }

    /// Services every request currently available on `queue`.
    ///
    /// A request that returns [`VirtioError::WouldBlock`] is returned to the
    /// caller as `Deferred` and must be retried before a later available-ring
    /// head. Other request errors are completed deterministically: errors with
    /// a valid status descriptor receive `VIRTIO_BLK_S_IOERR`; malformed
    /// chains are returned to the used ring with length zero.
    ///
    /// `pending_head` is a descriptor retained by the caller after an earlier
    /// deferred request. It is retried before this call consumes a new avail
    /// entry.
    pub fn process_queue<T: GuestMemoryAccessor + Clone>(
        &self,
        queue: &mut VirtioQueue<T>,
        memory: &mut dyn GuestMemory,
        pending_head: Option<u16>,
    ) -> VirtioResult<BlockQueueOutcome> {
        let mut completed = false;
        let mut notify = false;
        let mut pending_head = pending_head;

        loop {
            let head = if let Some(head) = pending_head.take() {
                head
            } else if let Some(head) = queue.pop_available_head_with_memory(memory)? {
                head
            } else if queue.rearm_available_event_with_memory(memory)? {
                continue;
            } else {
                break;
            };

            match self.process_request(queue, head, memory) {
                Ok(Some(written_len)) => {
                    completed = true;
                    notify |= queue.complete_with_memory(head, written_len, memory)?;
                }
                Ok(None) => {
                    return Ok(BlockQueueOutcome::Deferred {
                        pending_head: head,
                        notify,
                    });
                }
                Err(error) => {
                    warn!("virtio-blk request {head} failed: {error:?}");
                    completed = true;
                    notify |= queue.complete_with_memory(head, 0, memory)?;
                }
            }
        }

        Ok(if completed {
            BlockQueueOutcome::Completed { notify }
        } else {
            BlockQueueOutcome::Idle
        })
    }

    pub(crate) fn process_request<T: GuestMemoryAccessor + Clone>(
        &self,
        queue: &VirtioQueue<T>,
        head: u16,
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<Option<u32>> {
        let chain = queue.descriptor_chain_with_memory(head, memory)?;
        let descriptors = chain.descriptors();
        if descriptors.len() < MIN_DESCRIPTOR_CHAIN_LENGTH {
            return Err(VirtioError::InvalidDescriptor);
        }
        let (status, request_descriptors) = descriptors
            .split_last()
            .ok_or(VirtioError::InvalidDescriptor)?;
        let (header, data) = request_descriptors
            .split_first()
            .ok_or(VirtioError::InvalidDescriptor)?;
        self.validate_request_layout(header, data, status)?;

        let (request_type, sector) = read_header(header, memory)?;
        let completion = match request_type {
            VIRTIO_BLK_T_IN => self.process_read(sector, data, memory),
            VIRTIO_BLK_T_OUT => self.process_write(sector, data, memory),
            VIRTIO_BLK_T_FLUSH if data.is_empty() && self.config.flush_supported => {
                self.backend.flush().map(|()| 1)
            }
            _ => {
                memory.write(status.base_addr, &[VIRTIO_BLK_S_UNSUPP as u8])?;
                return Ok(Some(1));
            }
        };

        match completion {
            Ok(used_len) => {
                memory.write(status.base_addr, &[VIRTIO_BLK_S_OK as u8])?;
                Ok(Some(used_len))
            }
            Err(VirtioError::WouldBlock) => Ok(None),
            Err(error) => {
                warn!("virtio-blk request {head} failed: {error:?}");
                memory.write(status.base_addr, &[VIRTIO_BLK_S_IOERR as u8])?;
                Ok(Some(1))
            }
        }
    }

    fn validate_request_layout(
        &self,
        header: &VirtQueueDesc,
        data: &[VirtQueueDesc],
        status: &VirtQueueDesc,
    ) -> VirtioResult<()> {
        if header.is_write() || header.len < VIRTIO_BLK_REQUEST_HEADER_SIZE {
            return Err(VirtioError::InvalidDescriptor);
        }
        if !status.is_write() || status.len < 1 {
            return Err(VirtioError::InvalidDescriptor);
        }

        let header_range = descriptor_range(header)?;
        let status_range = descriptor_range(status)?;
        if ranges_overlap(&header_range, &status_range) {
            return Err(VirtioError::InvalidDescriptor);
        }
        for descriptor in data {
            let data_range = descriptor_range(descriptor)?;
            if ranges_overlap(&header_range, &data_range)
                || ranges_overlap(&status_range, &data_range)
            {
                return Err(VirtioError::InvalidDescriptor);
            }
        }
        Ok(())
    }

    fn process_read(
        &self,
        sector: u64,
        descriptors: &[VirtQueueDesc],
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<u32> {
        if !descriptors.iter().all(VirtQueueDesc::is_write) {
            return Err(VirtioError::InvalidDescriptor);
        }
        let total_len = self.validate_data_request(sector, descriptors)?;
        let mut buffer = allocate_request_buffer(total_len)?;
        let bytes_read = self.backend.read(sector, &mut buffer)?;
        if bytes_read != total_len {
            return Err(VirtioError::BackendError);
        }
        copy_to_guest(descriptors, &buffer, memory)?;
        u32::try_from(total_len)
            .ok()
            .and_then(|len| len.checked_add(1))
            .ok_or(VirtioError::InvalidBufferSize)
    }

    fn process_write(
        &self,
        sector: u64,
        descriptors: &[VirtQueueDesc],
        memory: &mut dyn GuestMemory,
    ) -> VirtioResult<u32> {
        if self.config.read_only {
            return Err(VirtioError::BackendError);
        }
        if !descriptors.iter().all(|descriptor| !descriptor.is_write()) {
            return Err(VirtioError::InvalidDescriptor);
        }
        let total_len = self.validate_data_request(sector, descriptors)?;
        let mut buffer = allocate_request_buffer(total_len)?;
        copy_from_guest(descriptors, &mut buffer, memory)?;
        let bytes_written = self.backend.write(sector, &buffer)?;
        if bytes_written != total_len {
            return Err(VirtioError::BackendError);
        }
        Ok(1)
    }

    fn validate_data_request(
        &self,
        sector: u64,
        descriptors: &[VirtQueueDesc],
    ) -> VirtioResult<usize> {
        if descriptors.len() > self.config.seg_max as usize
            || descriptors
                .iter()
                .any(|descriptor| descriptor.len > self.config.size_max)
        {
            return Err(VirtioError::InvalidBufferSize);
        }
        let total_len = descriptors.iter().try_fold(0usize, |total, descriptor| {
            total
                .checked_add(descriptor.len as usize)
                .ok_or(VirtioError::InvalidBufferSize)
        })?;
        if !total_len.is_multiple_of(SECTOR_SIZE as usize) {
            return Err(VirtioError::InvalidBufferSize);
        }
        let request_start = sector
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(VirtioError::InvalidSector)?;
        let request_end = request_start
            .checked_add(total_len as u64)
            .ok_or(VirtioError::InvalidSector)?;
        let capacity = self
            .config
            .capacity
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(VirtioError::InvalidConfig)?;
        if request_end > capacity {
            return Err(VirtioError::InvalidSector);
        }
        Ok(total_len)
    }
}

fn read_header(
    descriptor: &VirtQueueDesc,
    memory: &mut dyn GuestMemory,
) -> VirtioResult<(u32, u64)> {
    let mut bytes = [0u8; VIRTIO_BLK_REQUEST_HEADER_SIZE as usize];
    memory.read(descriptor.base_addr, &mut bytes)?;
    let mut request_type = [0u8; 4];
    request_type.copy_from_slice(&bytes[0..4]);
    let mut sector = [0u8; 8];
    sector.copy_from_slice(&bytes[8..16]);
    Ok((u32::from_le_bytes(request_type), u64::from_le_bytes(sector)))
}

fn copy_to_guest(
    descriptors: &[VirtQueueDesc],
    buffer: &[u8],
    memory: &mut dyn GuestMemory,
) -> VirtioResult<()> {
    let mut offset = 0usize;
    for descriptor in descriptors {
        let end = offset
            .checked_add(descriptor.len as usize)
            .ok_or(VirtioError::InvalidBufferSize)?;
        memory.write(descriptor.base_addr, &buffer[offset..end])?;
        offset = end;
    }
    Ok(())
}

fn copy_from_guest(
    descriptors: &[VirtQueueDesc],
    buffer: &mut [u8],
    memory: &mut dyn GuestMemory,
) -> VirtioResult<()> {
    let mut offset = 0usize;
    for descriptor in descriptors {
        let end = offset
            .checked_add(descriptor.len as usize)
            .ok_or(VirtioError::InvalidBufferSize)?;
        memory.read(descriptor.base_addr, &mut buffer[offset..end])?;
        offset = end;
    }
    Ok(())
}

fn descriptor_range(descriptor: &VirtQueueDesc) -> VirtioResult<Range<usize>> {
    let start = descriptor.base_addr.as_usize();
    let end = start
        .checked_add(descriptor.len as usize)
        .ok_or(VirtioError::InvalidDescriptor)?;
    Ok(start..end)
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn allocate_request_buffer(len: usize) -> VirtioResult<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| VirtioError::InvalidBufferSize)?;
    buffer.resize(len, 0);
    Ok(buffer)
}
