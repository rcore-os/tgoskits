use alloc::{sync::Arc, vec::Vec};

use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    MmioReadOutcome, MmioWriteAction, VirtioDeviceID, VirtioError, VirtioMmioState, VirtioQueue,
    VirtioResult, mmio::transport,
};
use axvm_types::{AccessWidth, GuestPhysAddr};

use crate::{
    backend::BlockBackend,
    block::{BlockRequest, config::VirtioBlockConfig, request::BlockRequestResult},
    constants::*,
    mmio::VirtioBlockHeader,
};

/// VirtIO MMIO Block Device
///
/// Standard MMIO register handling is delegated to [`VirtioMmioState`]; this
/// type owns only the block-specific configuration space and request data path.
///
/// # Generic Parameters
/// - `B`: Block backend implementation that handles actual storage operations
/// - `T`: Guest memory accessor with address translation capabilities
pub struct VirtioMmioBlockDevice<B: BlockBackend, T: GuestMemoryAccessor + Clone> {
    /// Shared VirtIO MMIO transport state and queues.
    state: VirtioMmioState<T>,
    /// Block device configuration (capacity, geometry, ...).
    block_config: VirtioBlockConfig,
    /// Block backend.
    backend: B,
    /// Guest memory accessor.
    accessor: Arc<T>,
}

impl<B: BlockBackend, T: GuestMemoryAccessor + Clone> VirtioMmioBlockDevice<B, T> {
    /// Create a new VirtIO MMIO block device.
    pub fn new(
        base_ipa: GuestPhysAddr,
        length: usize,
        block_backend: B,
        block_config: VirtioBlockConfig,
        translator: T,
    ) -> VirtioResult<Self> {
        let accessor = Arc::new(translator);
        let queues = vec![VirtioQueue::new(0, DEFAULT_QUEUE_SIZE, accessor.clone())];

        let state = VirtioMmioState::new(
            base_ipa,
            length,
            VirtioDeviceID::Block.to_device_id(),
            VIRTIO_VENDOR_ID,
            VIRTIO_BLK_FEATURES,
            queues,
        );

        Ok(Self {
            state,
            block_config,
            backend: block_backend,
            accessor,
        })
    }

    /// Check if device is enabled.
    pub fn is_enabled(&self) -> bool {
        true
    }

    /// Get device status.
    pub fn get_status(&self) -> u32 {
        self.state.status()
    }

    /// Set device status directly (bypasses validation; bring-up helper).
    pub fn set_status(&self, status: u32) {
        self.state.set_status(status);
    }

    /// Check if device is ready (driver has set `DRIVER_OK`).
    pub fn is_device_ready(&self) -> bool {
        self.state.is_driver_ok()
    }

    /// Handle MMIO read operations. Standard registers are served by the shared
    /// state; the block config region is interpreted here.
    pub fn mmio_read(&self, addr: GuestPhysAddr, width: AccessWidth) -> VirtioResult<usize> {
        if !self.is_enabled() {
            return Ok(0);
        }
        match self.state.mmio_read(addr, width)? {
            MmioReadOutcome::Standard(v) => Ok(v as usize),
            MmioReadOutcome::DeviceConfig { offset, width } => {
                self.read_config_space(offset, width)
            }
        }
    }

    /// Handle MMIO write operations.
    pub fn mmio_write(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
    ) -> VirtioResult<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        match self.state.mmio_write(addr, width, val)? {
            MmioWriteAction::None => {}
            MmioWriteAction::Reset => {}
            MmioWriteAction::QueueNotified(queue_index) => {
                self.handle_queue_notify(queue_index);
            }
        }
        Ok(())
    }

    /// Handle queue notification.
    fn handle_queue_notify(&self, queue_index: u16) {
        if !self.is_device_ready() {
            warn!("Device not ready, ignoring queue notification");
            return;
        }

        // Get a copy of the queue to avoid holding the lock during processing.
        let queue_copy = {
            let queues = self.state.queues_lock();
            match queues.get(queue_index as usize) {
                Some(q) if q.ready => q.clone(),
                Some(_) => {
                    warn!("Queue {} not ready", queue_index);
                    return;
                }
                None => {
                    warn!("Invalid queue index: {}", queue_index);
                    return;
                }
            }
        };

        // Check if queue addresses are set.
        if queue_copy.desc_table_addr.as_usize() == 0
            || queue_copy.avail_ring_addr.as_usize() == 0
            || queue_copy.used_ring_addr.as_usize() == 0
        {
            warn!("Queue {} addresses not properly set", queue_index);
            return;
        }

        self.process_queue_requests(&queue_copy);
    }

    /// Process requests in the queue.
    fn process_queue_requests(&self, queue: &VirtioQueue<T>) {
        let avail_idx = match queue.read_avail_idx() {
            Ok(idx) => idx,
            Err(e) => {
                error!("Failed to read available index: {:?}", e);
                return;
            }
        };

        trace!(
            "Available index: {}, next_avail: {}",
            avail_idx,
            queue.get_last_avail_idx()
        );

        let mut current_avail = queue.get_last_avail_idx();
        let mut processed_requests = Vec::new();

        while current_avail != avail_idx {
            let ring_index = current_avail % queue.size;
            let desc_index = match queue.read_avail_entry(ring_index) {
                Ok(idx) => idx,
                Err(e) => {
                    error!(
                        "Failed to read available ring entry {}: {:?}",
                        ring_index, e
                    );
                    current_avail = current_avail.wrapping_add(1);
                    continue;
                }
            };

            trace!(
                "Processing descriptor chain starting at index {}",
                desc_index
            );

            match self.process_descriptor_chain(queue, desc_index) {
                Ok(()) => {}
                Err(e) => {
                    error!("Failed to process descriptor chain {}: {:?}", desc_index, e);
                    if let Err(se) = queue.write_status_byte(desc_index, VIRTIO_BLK_S_IOERR as u8) {
                        error!("Failed to write error status byte: {:?}", se);
                    }
                    processed_requests.push((desc_index, 0u32));
                }
            }

            current_avail = current_avail.wrapping_add(1);
        }

        if current_avail != queue.get_last_avail_idx() || !processed_requests.is_empty() {
            let processed_count = current_avail.wrapping_sub(queue.get_last_avail_idx());
            trace!("Processed {} requests", processed_count);

            let mut queues = self.state.queues_lock();
            if let Some(queue_mut) = queues.get_mut(queue.index as usize) {
                queue_mut.update_last_avail_idx(current_avail);

                for (desc_index, len) in processed_requests {
                    if let Err(e) = queue_mut.add_used(desc_index, len) {
                        error!("Failed to add used buffer for error request: {:?}", e);
                    }
                }

                let notify = queue_mut.should_notify().unwrap_or(false);
                if notify {
                    drop(queues);
                    self.trigger_interrupt();
                }
            }
        }
    }

    /// Process a descriptor chain.
    fn process_descriptor_chain(
        &self,
        queue: &VirtioQueue<T>,
        head_index: u16,
    ) -> VirtioResult<()> {
        let request = self.parse_virtio_request(queue, head_index)?;
        let status = self.execute_block_request(&request)?;
        let request_size = request.size() as u32;
        self.add_used_buffer(queue, head_index, request_size, status);
        Ok(())
    }

    /// Parse VirtIO block request from descriptor chain.
    fn parse_virtio_request(
        &self,
        queue: &VirtioQueue<T>,
        head_index: u16,
    ) -> VirtioResult<BlockRequest<T>> {
        let header = match self.parse_virtio_block_header(queue, head_index) {
            Ok(header) => header,
            Err(e) => {
                error!("Failed to parse VirtIO block header: {:?}", e);
                return Err(VirtioError::InvalidQueue);
            }
        };

        match queue.validate_virtio_block_chain(head_index, MIN_DESCRIPTOR_CHAIN_LENGTH) {
            Ok(true) => {}
            Ok(false) => {
                error!("Invalid VirtIO block descriptor chain");
                return Err(VirtioError::InvalidQueue);
            }
            Err(e) => {
                error!("Failed to validate descriptor chain: {:?}", e);
                return Err(VirtioError::InvalidQueue);
            }
        }

        let buffers = match queue.get_data_buffers(head_index, VirtioDeviceID::Block) {
            Ok(buffers) => buffers,
            Err(e) => {
                error!("Failed to get data buffers: {:?}", e);
                return Err(VirtioError::InvalidQueue);
            }
        };

        trace!("Descriptor chain has {} data buffers", buffers.len());

        let status_addr = match queue.get_status_addr(head_index) {
            Ok(addr) => addr,
            Err(e) => {
                error!("Failed to get status address: {:?}", e);
                return Err(VirtioError::InvalidQueue);
            }
        };

        let request = BlockRequest::new_virtio(
            header.request_type,
            header.sector,
            buffers,
            status_addr,
            self.accessor.clone(),
        );

        Ok(request)
    }

    /// Parse VirtIO block header.
    pub fn parse_virtio_block_header(
        &self,
        queue: &VirtioQueue<T>,
        head_index: u16,
    ) -> VirtioResult<VirtioBlockHeader> {
        if let Some(ref desc_table) = queue.desc_table {
            let descriptors = desc_table.follow_chain(head_index)?;
            if descriptors.is_empty() {
                return Err(VirtioError::InvalidDescriptor);
            }

            let header_desc = &descriptors[0];

            if header_desc.is_write() {
                warn!("Request header descriptor should not be write-only");
                return Err(VirtioError::InvalidDescriptor);
            }

            if header_desc.len < VirtioBlockHeader::SIZE {
                warn!(
                    "Request header descriptor too small: {} bytes, need {} bytes",
                    header_desc.len,
                    VirtioBlockHeader::SIZE
                );
                return Err(VirtioError::InvalidDescriptor);
            }

            let header_addr = header_desc.guest_addr();
            let header = VirtioBlockHeader::read_from_guest(header_addr, self.accessor.clone())?;

            trace!(
                "Parsed VirtIO block header: type={}, sector={}",
                header.request_type, header.sector
            );

            Ok(header)
        } else {
            Err(VirtioError::QueueNotReady)
        }
    }

    /// Execute a block request.
    fn execute_block_request(&self, request: &BlockRequest<T>) -> VirtioResult<u8> {
        match request.execute(&self.backend) {
            Ok(status) => Ok(status as u8),
            Err(e) => {
                error!("Block request execution failed: {:?}", e);
                let status = match e {
                    VirtioError::InvalidBufferSize => BlockRequestResult::Unsupported,
                    VirtioError::MemoryError => BlockRequestResult::IoError,
                    _ => BlockRequestResult::IoError,
                };
                Ok(status as u8)
            }
        }
    }

    /// Add a used buffer to the used ring.
    fn add_used_buffer(&self, queue: &VirtioQueue<T>, desc_index: u16, len: u32, status: u8) {
        trace!(
            "Completing request: desc_index={}, len={}, status={}",
            desc_index, len, status
        );

        if let Err(e) = queue.write_status_byte(desc_index, status) {
            error!("Failed to write status byte: {:?}", e);
            return;
        }

        let mut queues = self.state.queues_lock();
        if let Some(queue_mut) = queues.get_mut(queue.index as usize) {
            if let Err(e) = queue_mut.add_used(desc_index, len) {
                error!("Failed to add used buffer: {:?}", e);
                return;
            }

            match queue_mut.should_notify() {
                Ok(should_notify) => {
                    if should_notify {
                        drop(queues);
                        self.trigger_interrupt();
                    }
                }
                Err(e) => {
                    error!("Failed to check notification requirement: {:?}", e);
                }
            }
        } else {
            error!("Invalid queue index: {}", queue.index);
        }
    }

    /// Raise the used-buffer notification interrupt bit.
    fn trigger_interrupt(&self) {
        self.state.set_interrupt(VIRTIO_MMIO_INT_VRING);
        trace!("Triggered interrupt for used buffer notification");
    }

    /// Get the currently selected queue index, if in range.
    pub fn get_selected_queue(&self) -> Option<u16> {
        self.state.selected_queue_index()
    }

    /// Get a clone of the queue at `index`, if it exists.
    pub fn get_queue(&self, index: u16) -> Option<VirtioQueue<T>> {
        self.state.queues_lock().get(index as usize).cloned()
    }

    /// Read from block device configuration space.
    fn read_config_space(&self, offset: u64, width: AccessWidth) -> VirtioResult<usize> {
        // Block config space uses 32-bit accesses.
        transport::validate_access_width(width)?;

        let value = match offset {
            VIRTIO_BLK_CFG_CAPACITY_LOW => self.block_config.capacity as u32,
            VIRTIO_BLK_CFG_CAPACITY_HIGH => (self.block_config.capacity >> 32) as u32,
            VIRTIO_BLK_CFG_SIZE_MAX => self.block_config.size_max,
            VIRTIO_BLK_CFG_SEG_MAX => self.block_config.seg_max,
            VIRTIO_BLK_CFG_GEOMETRY => {
                (self.block_config.cylinders as u32)
                    | ((self.block_config.heads as u32) << 16)
                    | ((self.block_config.sectors as u32) << 24)
            }
            VIRTIO_BLK_CFG_BLK_SIZE => self.block_config.blk_size,
            VIRTIO_BLK_CFG_PHYSICAL_BLOCK_EXP => self.block_config.physical_block_exp as u32,
            VIRTIO_BLK_CFG_ALIGNMENT_OFFSET => self.block_config.alignment_offset as u32,
            VIRTIO_BLK_CFG_MIN_IO_SIZE => self.block_config.min_io_size as u32,
            VIRTIO_BLK_CFG_OPT_IO_SIZE => self.block_config.opt_io_size,
            _ => 0,
        };

        Ok(value as usize)
    }
}
