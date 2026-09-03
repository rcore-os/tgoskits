use alloc::{sync::Arc, vec};

use ax_sync::SpinLock;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    AddressSpaceMemory, MmioReadOutcome, MmioWriteAction, VirtioDeviceID, VirtioError,
    VirtioMmioState, VirtioQueue, VirtioResult, mmio::transport, queue::VirtQueueDesc,
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
    /// A blocking backend must process the notified queue from runtime poll.
    QueuePending(u16),
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
    pending_head: SpinLock<Option<u16>>,
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
            pending_head: SpinLock::new(None),
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
    ///
    /// The capability backs the `QUEUE_READY` ring-layout validation as well
    /// as queue processing, so runtimes whose queues were constructed with a
    /// non-translating placeholder accessor (e.g. axvisor) can still make
    /// queues ready.
    pub fn mmio_write_with_memory(
        &self,
        addr: GuestPhysAddr,
        width: AccessWidth,
        val: usize,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<BlockDeviceEvent> {
        match self
            .state
            .mmio_write_with_memory(addr, width, val, memory)?
        {
            MmioWriteAction::None => Ok(BlockDeviceEvent::None),
            MmioWriteAction::Reset => {
                // A transport reset invalidates every in-flight descriptor,
                // including a deferred request that was removed from avail.
                self.pending_head.lock().take();
                Ok(BlockDeviceEvent::Reset)
            }
            MmioWriteAction::InterruptPending => Ok(BlockDeviceEvent::InterruptPending),
            MmioWriteAction::QueueNotified(queue_index) => {
                if self.backend.requires_deferred_processing() {
                    Ok(BlockDeviceEvent::QueuePending(queue_index))
                } else {
                    self.handle_queue_notify(queue_index, memory)
                }
            }
        }
    }

    /// Processes one previously notified queue from a runtime context that may
    /// block while servicing the backend.
    pub fn process_pending_queue(
        &self,
        queue_index: u16,
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<BlockDeviceEvent> {
        self.handle_queue_notify(queue_index, memory)
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
        let mut pending_head = self.pending_head.lock();
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
                Ok(Some(written)) => {
                    notify |= queue.complete_with_memory(head, written, memory)?;
                }
                Ok(None) => {
                    *pending_head = Some(head);
                    return Ok(BlockDeviceEvent::QueuePending(queue_index));
                }
                Err(error) => {
                    warn!("virtio-blk request {head} failed: {error:?}");
                    notify |= queue.complete_with_memory(head, 0, memory)?;
                }
            }
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
    ) -> VirtioResult<Option<u32>> {
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
        let completion = match request_type {
            VIRTIO_BLK_T_IN => self.process_read(sector, data, memory),
            VIRTIO_BLK_T_OUT => self.process_write(sector, data, memory),
            VIRTIO_BLK_T_FLUSH if data.is_empty() => self.backend.flush().map(|()| 1),
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

    fn process_read(
        &self,
        sector: u64,
        descriptors: &[VirtQueueDesc],
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<u32> {
        if !descriptors.iter().all(|descriptor| descriptor.is_write()) {
            return Err(VirtioError::InvalidDescriptor);
        }
        let total_len = self.validate_data_request(sector, descriptors)?;
        let mut buffer = allocate_request_buffer(total_len)?;
        let bytes_read = self.backend.read(sector, &mut buffer)?;
        if bytes_read != total_len {
            return Err(VirtioError::BackendError);
        }
        let mut offset = 0;
        for descriptor in descriptors {
            let end = offset + descriptor.len as usize;
            memory.write(descriptor.base_addr, &buffer[offset..end])?;
            offset = end;
        }
        u32::try_from(total_len)
            .ok()
            .and_then(|len| len.checked_add(1))
            .ok_or(VirtioError::InvalidBufferSize)
    }

    fn process_write(
        &self,
        sector: u64,
        descriptors: &[VirtQueueDesc],
        memory: &mut dyn axvirtio_common::GuestMemory,
    ) -> VirtioResult<u32> {
        if !descriptors.iter().all(|descriptor| !descriptor.is_write()) {
            return Err(VirtioError::InvalidDescriptor);
        }
        let total_len = self.validate_data_request(sector, descriptors)?;
        let mut buffer = allocate_request_buffer(total_len)?;
        let mut offset = 0;
        for descriptor in descriptors {
            let end = offset + descriptor.len as usize;
            memory.read(descriptor.base_addr, &mut buffer[offset..end])?;
            offset = end;
        }
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
        if descriptors.len() > self.block_config.seg_max as usize
            || descriptors
                .iter()
                .any(|descriptor| descriptor.len > self.block_config.size_max)
        {
            return Err(VirtioError::InvalidBufferSize);
        }
        let total_len = descriptors.iter().try_fold(0usize, |total, descriptor| {
            total
                .checked_add(descriptor.len as usize)
                .ok_or(VirtioError::InvalidBufferSize)
        })?;
        if total_len % SECTOR_SIZE as usize != 0 {
            return Err(VirtioError::InvalidBufferSize);
        }
        let request_start = sector
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(VirtioError::InvalidSector)?;
        let request_end = request_start
            .checked_add(total_len as u64)
            .ok_or(VirtioError::InvalidSector)?;
        let capacity = self
            .block_config
            .capacity
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(VirtioError::InvalidConfig)?;
        if request_end > capacity {
            return Err(VirtioError::InvalidSector);
        }
        Ok(total_len)
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

fn allocate_request_buffer(len: usize) -> VirtioResult<alloc::vec::Vec<u8>> {
    let mut buffer = alloc::vec::Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| VirtioError::InvalidBufferSize)?;
    buffer.resize(len, 0);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};

    use axvirtio_common::{
        GuestMemory, NoGuestMemoryAccessor,
        constants::{VIRTIO_MMIO_STATUS, VIRTQ_DESC_F_NEXT},
    };

    use super::*;

    const DESC_TABLE: usize = 0x100;
    const AVAIL_RING: usize = 0x200;
    const USED_RING: usize = 0x240;
    const HEADER: usize = 0x300;
    const DATA: usize = 0x400;
    const STATUS: usize = 0x800;
    const VIRTQ_DESC_F_WRITE: u16 = 2;
    const IOERR: u8 = 1;

    struct TestMemory(Vec<u8>);

    impl TestMemory {
        fn new() -> Self {
            Self(vec![0; 0x1000])
        }

        fn set_descriptor(&mut self, index: usize, addr: usize, len: u32, flags: u16, next: u16) {
            let offset = DESC_TABLE + index * 16;
            self.0[offset..offset + 8].copy_from_slice(&(addr as u64).to_le_bytes());
            self.0[offset + 8..offset + 12].copy_from_slice(&len.to_le_bytes());
            self.0[offset + 12..offset + 14].copy_from_slice(&flags.to_le_bytes());
            self.0[offset + 14..offset + 16].copy_from_slice(&next.to_le_bytes());
        }

        fn set_header(&mut self, request_type: u32, sector: u64) {
            self.0[HEADER..HEADER + 4].copy_from_slice(&request_type.to_le_bytes());
            self.0[HEADER + 8..HEADER + 16].copy_from_slice(&sector.to_le_bytes());
        }
    }

    impl GuestMemory for TestMemory {
        fn read(&mut self, guest_addr: GuestPhysAddr, data: &mut [u8]) -> VirtioResult<()> {
            let start = guest_addr.as_usize();
            let source = self
                .0
                .get(start..start + data.len())
                .ok_or(VirtioError::InvalidAddress)?;
            data.copy_from_slice(source);
            Ok(())
        }

        fn write(&mut self, guest_addr: GuestPhysAddr, data: &[u8]) -> VirtioResult<()> {
            let start = guest_addr.as_usize();
            let destination = self
                .0
                .get_mut(start..start + data.len())
                .ok_or(VirtioError::InvalidAddress)?;
            destination.copy_from_slice(data);
            Ok(())
        }
    }

    struct TestBackend;

    impl BlockBackend for TestBackend {
        fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            if sector >= 8 {
                return Err(VirtioError::InvalidSector);
            }
            buffer.fill(0x5a);
            Ok(buffer.len())
        }

        fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
            if sector >= 8 {
                return Err(VirtioError::InvalidSector);
            }
            Ok(buffer.len())
        }

        fn flush(&self) -> VirtioResult<()> {
            Ok(())
        }
    }

    fn fixture(
        data_len: u32,
        sector: u64,
    ) -> (
        VirtioMmioBlockDevice<TestBackend, NoGuestMemoryAccessor>,
        VirtioQueue<NoGuestMemoryAccessor>,
        TestMemory,
    ) {
        let config = VirtioBlockConfig {
            capacity: 8,
            size_max: 64,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let device = VirtioMmioBlockDevice::new(
            GuestPhysAddr::from(0x0a00_0000),
            0x200,
            TestBackend,
            config,
            NoGuestMemoryAccessor,
        )
        .unwrap();
        let accessor = Arc::new(NoGuestMemoryAccessor);
        let mut queue = VirtioQueue::new(0, 4, accessor);
        queue
            .set_desc_table_addr(GuestPhysAddr::from(DESC_TABLE))
            .unwrap();
        let mut memory = TestMemory::new();
        memory.set_descriptor(0, HEADER, VirtioBlockHeader::SIZE, VIRTQ_DESC_F_NEXT, 1);
        memory.set_descriptor(1, DATA, data_len, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, sector);
        memory.0[STATUS] = 0xff;
        (device, queue, memory)
    }

    #[test]
    fn oversized_segment_completes_with_ioerr_without_allocating_guest_length() {
        let (device, queue, mut memory) = fixture(65, 0);

        assert_eq!(device.process_request(&queue, 0, &mut memory), Ok(Some(1)));
        assert_eq!(memory.0[STATUS], IOERR);
    }

    #[test]
    fn out_of_capacity_request_completes_with_ioerr() {
        let (device, queue, mut memory) = fixture(64, 8);

        assert_eq!(device.process_request(&queue, 0, &mut memory), Ok(Some(1)));
        assert_eq!(memory.0[STATUS], IOERR);
    }

    #[test]
    fn reset_discards_deferred_request_head() {
        let (device, _queue, mut memory) = fixture(64, 0);
        *device.pending_head.lock() = Some(3);

        let event = device.mmio_write_with_memory(
            GuestPhysAddr::from(0x0a00_0000 + VIRTIO_MMIO_STATUS),
            AccessWidth::Dword,
            0,
            &mut memory,
        );

        assert_eq!(event, Ok(BlockDeviceEvent::Reset));
        assert_eq!(*device.pending_head.lock(), None);
    }

    #[test]
    fn empty_event_idx_queue_rearms_the_next_available_index() {
        let (device, _queue, mut memory) = fixture(64, 0);
        device.set_status(crate::constants::VIRTIO_STATUS_DRIVER_OK);
        {
            let mut queues = device.state.queues_lock();
            let queue = &mut queues[0];
            queue.set_size(4).unwrap();
            queue
                .set_desc_table_addr(GuestPhysAddr::from(DESC_TABLE))
                .unwrap();
            queue
                .set_avail_ring_addr(GuestPhysAddr::from(AVAIL_RING))
                .unwrap();
            queue
                .set_used_ring_addr(GuestPhysAddr::from(USED_RING))
                .unwrap();
            queue.set_ready(true);
            queue.event_idx_enabled = true;
            queue.update_last_avail_idx(2);
        }
        memory.0[AVAIL_RING + 2..AVAIL_RING + 4].copy_from_slice(&2u16.to_le_bytes());

        assert_eq!(
            device.handle_queue_notify(0, &mut memory),
            Ok(BlockDeviceEvent::None)
        );

        let avail_event = USED_RING + 4 + 4 * 8;
        assert_eq!(
            u16::from_le_bytes(memory.0[avail_event..avail_event + 2].try_into().unwrap()),
            2
        );
    }
}
