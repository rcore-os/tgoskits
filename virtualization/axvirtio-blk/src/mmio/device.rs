use alloc::{sync::Arc, vec};

use ax_sync::SpinLock;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    AddressSpaceMemory, MmioReadOutcome, MmioWriteAction, VirtioDeviceID, VirtioError,
    VirtioMmioState, VirtioQueue, VirtioResult, mmio::transport,
};
use axvm_types::{AccessWidth, GuestPhysAddr};
use log::trace;

use crate::{
    backend::BlockBackend,
    block::{BlockQueueOutcome, VirtioBlockRequestCore, config::VirtioBlockConfig},
    constants::*,
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
    /// Transport-independent block request processing.
    core: VirtioBlockRequestCore<B>,
    /// Deferred request head owned by this MMIO transport.
    pending_head: SpinLock<Option<u16>>,
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

        let mut device_features = VIRTIO_BLK_FEATURES;
        if block_config.read_only {
            device_features |= VIRTIO_BLK_F_RO;
        }
        if !block_config.flush_supported {
            device_features &= !VIRTIO_BLK_F_FLUSH;
        }
        let state = VirtioMmioState::new(
            base_ipa,
            length,
            VirtioDeviceID::Block.to_device_id(),
            VIRTIO_VENDOR_ID,
            device_features,
            queues,
        );

        Ok(Self {
            state,
            core: VirtioBlockRequestCore::new(block_backend, block_config),
            pending_head: SpinLock::new(None),
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
                self.clear_pending_head();
                Ok(BlockDeviceEvent::Reset)
            }
            MmioWriteAction::InterruptPending => Ok(BlockDeviceEvent::InterruptPending),
            MmioWriteAction::QueueNotified(index) => {
                if self.core.requires_deferred_processing() {
                    Ok(BlockDeviceEvent::QueuePending(index))
                } else {
                    self.handle_queue_notify(index, memory)
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
        let pending_head = self.take_pending_head();
        let outcome = self.core.process_queue(queue, memory, pending_head);
        drop(queues);
        match outcome? {
            BlockQueueOutcome::Idle | BlockQueueOutcome::Completed { notify: false } => {
                Ok(BlockDeviceEvent::None)
            }
            BlockQueueOutcome::Completed { notify: true } => {
                self.trigger_interrupt();
                Ok(BlockDeviceEvent::InterruptPending)
            }
            BlockQueueOutcome::Deferred {
                pending_head,
                notify,
            } => {
                self.store_pending_head(pending_head);
                if notify {
                    self.trigger_interrupt();
                }
                Ok(BlockDeviceEvent::QueuePending(queue_index))
            }
        }
    }

    fn take_pending_head(&self) -> Option<u16> {
        self.pending_head.lock().take()
    }

    fn store_pending_head(&self, head: u16) {
        *self.pending_head.lock() = Some(head);
    }

    fn clear_pending_head(&self) {
        self.pending_head.lock().take();
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
            VIRTIO_BLK_CFG_CAPACITY_LOW => self.core.config().capacity as u32,
            VIRTIO_BLK_CFG_CAPACITY_HIGH => (self.core.config().capacity >> 32) as u32,
            VIRTIO_BLK_CFG_SIZE_MAX => self.core.config().size_max,
            VIRTIO_BLK_CFG_SEG_MAX => self.core.config().seg_max,
            VIRTIO_BLK_CFG_GEOMETRY => {
                (self.core.config().cylinders as u32)
                    | ((self.core.config().heads as u32) << 16)
                    | ((self.core.config().sectors as u32) << 24)
            }
            VIRTIO_BLK_CFG_BLK_SIZE => self.core.config().blk_size,
            VIRTIO_BLK_CFG_PHYSICAL_BLOCK_EXP => self.core.config().physical_block_exp as u32,
            VIRTIO_BLK_CFG_ALIGNMENT_OFFSET => self.core.config().alignment_offset as u32,
            VIRTIO_BLK_CFG_MIN_IO_SIZE => self.core.config().min_io_size as u32,
            VIRTIO_BLK_CFG_OPT_IO_SIZE => self.core.config().opt_io_size,
            _ => 0,
        };

        Ok(value as usize)
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use axvirtio_common::{
        GuestMemory, NoGuestMemoryAccessor, VirtioError,
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

    static WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);

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
            WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
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
            read_only: true,
            capacity: 8,
            size_max: 512,
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
        memory.set_descriptor(
            0,
            HEADER,
            crate::block::VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, data_len, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, sector);
        memory.0[STATUS] = 0xff;
        (device, queue, memory)
    }

    #[test]
    fn oversized_segment_completes_with_ioerr_without_allocating_guest_length() {
        let (device, queue, mut memory) = fixture(513, 0);

        assert_eq!(
            device.core.process_request(&queue, 0, &mut memory),
            Ok(Some(1))
        );
        assert_eq!(memory.0[STATUS], IOERR);
    }

    #[test]
    fn out_of_capacity_request_completes_with_ioerr() {
        let (device, queue, mut memory) = fixture(512, 8);

        assert_eq!(
            device.core.process_request(&queue, 0, &mut memory),
            Ok(Some(1))
        );
        assert_eq!(memory.0[STATUS], IOERR);
    }

    #[test]
    fn read_only_policy_rejects_out_without_calling_backend() {
        WRITE_CALLS.store(0, Ordering::Relaxed);
        let (device, queue, mut memory) = fixture(512, 0);

        assert_eq!(
            device.core.process_request(&queue, 0, &mut memory),
            Ok(Some(1))
        );
        assert_eq!(memory.0[STATUS], IOERR);
        assert_eq!(WRITE_CALLS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn reset_discards_deferred_request_head() {
        let (device, _queue, mut memory) = fixture(64, 0);
        device.store_pending_head(3);

        let event = device.mmio_write_with_memory(
            GuestPhysAddr::from(0x0a00_0000 + VIRTIO_MMIO_STATUS),
            AccessWidth::Dword,
            0,
            &mut memory,
        );

        assert_eq!(event, Ok(BlockDeviceEvent::Reset));
        assert_eq!(device.take_pending_head(), None);
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
