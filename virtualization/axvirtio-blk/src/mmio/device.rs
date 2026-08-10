use alloc::{sync::Arc, vec};

use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    AddressSpaceMemory, MmioReadOutcome, MmioWriteAction, VirtioDeviceID, VirtioError,
    VirtioMmioState, VirtioQueue, VirtioResult, mmio::transport,
};
use axvm_types::{AccessWidth, GuestPhysAddr};
use log::{trace, warn};

use crate::{
    backend::BlockBackend, block::config::VirtioBlockConfig, constants::*, mmio::VirtioBlockHeader,
};

/// Action that the VMM must perform after an MMIO write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockDeviceEvent {
    /// No external action is required.
    None,
    /// The used-ring interrupt bit became pending.
    InterruptPending,
    /// The guest reset the transport.
    Reset,
}

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

    /// Returns the current VirtIO MMIO interrupt status bits.
    pub fn interrupt_status(&self) -> u32 {
        self.state.interrupt_status()
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
    ) -> VirtioResult<BlockDeviceEvent> {
        let mut memory = AddressSpaceMemory::new(self.accessor.as_ref());
        self.mmio_write_with_memory(addr, width, val, &mut memory)
    }

    /// Handles an MMIO write using a guest-memory capability scoped to this
    /// device access.
    pub fn mmio_write_with_memory(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<BlockDeviceEvent> {
        match self.state.mmio_write(addr, width, val)? {
            MmioWriteAction::None => Ok(BlockDeviceEvent::None),
            MmioWriteAction::Reset => Ok(BlockDeviceEvent::Reset),
            MmioWriteAction::InterruptPending => Ok(BlockDeviceEvent::InterruptPending),
            MmioWriteAction::QueueNotified(queue_index) => {
                self.handle_queue_notify(queue_index, memory)
            }
        }
    }

    /// Handle queue notification.
    fn handle_queue_notify(
        &self,
        queue_index: u16,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<BlockDeviceEvent> {
        if !self.is_device_ready() {
            return Ok(BlockDeviceEvent::None);
        }
        let mut queues = self.state.queues_lock();
        let queue = queues
            .get_mut(queue_index as usize)
            .ok_or(VirtioError::InvalidQueue)?;
        if !queue.is_valid() {
            return Ok(BlockDeviceEvent::None);
        }
        let mut notify = false;
        while let Some(head) = queue.pop_available_head_with_memory(memory)? {
            let written = self
                .process_request(queue, head, memory)
                .unwrap_or_else(|error| {
                    warn!("virtio-blk request {head} failed: {error:?}");
                    0
                });
            notify |= queue.complete_with_memory(head, written, memory)?;
        }
        if notify {
            self.trigger_interrupt();
            Ok(BlockDeviceEvent::InterruptPending)
        } else {
            Ok(BlockDeviceEvent::None)
        }
    }

    fn process_request(
        &self,
        queue: &VirtioQueue<T>,
        head: u16,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<u32> {
        let chain = queue.descriptor_chain_with_memory(head, memory)?;
        let descriptors = chain.descriptors();
        if descriptors.len() < MIN_DESCRIPTOR_CHAIN_LENGTH {
            return Err(VirtioError::InvalidDescriptor);
        }
        let header = &descriptors[0];
        let status = descriptors.last().unwrap();
        if header.is_write() || header.len < VirtioBlockHeader::SIZE {
            return Err(VirtioError::InvalidDescriptor);
        }
        if !status.is_write() || status.len < 1 {
            return Err(VirtioError::InvalidDescriptor);
        }
        let mut header_bytes = [0u8; VirtioBlockHeader::SIZE as usize];
        memory.read(header.base_addr, &mut header_bytes)?;
        let request_type = u32::from_le_bytes(header_bytes[0..4].try_into().unwrap());
        let sector = u64::from_le_bytes(header_bytes[8..16].try_into().unwrap());
        let data = &descriptors[1..descriptors.len() - 1];
        let total_len = data.iter().try_fold(0usize, |total, descriptor| {
            total
                .checked_add(descriptor.len as usize)
                .ok_or(VirtioError::InvalidBufferSize)
        })?;
        let mut buffer = vec![0u8; total_len];
        let result = match request_type {
            VIRTIO_BLK_T_IN if data.iter().all(|descriptor| descriptor.is_write()) => {
                self.backend.read(sector, &mut buffer)?;
                let mut offset = 0;
                for descriptor in data {
                    let end = offset + descriptor.len as usize;
                    memory.write(descriptor.base_addr, &buffer[offset..end])?;
                    offset = end;
                }
                total_len as u32 + 1
            }
            VIRTIO_BLK_T_OUT if data.iter().all(|descriptor| !descriptor.is_write()) => {
                let mut offset = 0;
                for descriptor in data {
                    let end = offset + descriptor.len as usize;
                    memory.read(descriptor.base_addr, &mut buffer[offset..end])?;
                    offset = end;
                }
                self.backend.write(sector, &buffer)?;
                1
            }
            VIRTIO_BLK_T_FLUSH if data.is_empty() => {
                self.backend.flush()?;
                1
            }
            _ => {
                memory.write(status.base_addr, &[VIRTIO_BLK_S_UNSUPP as u8])?;
                return Ok(1);
            }
        };
        memory.write(status.base_addr, &[VIRTIO_BLK_S_OK as u8])?;
        Ok(result)
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
