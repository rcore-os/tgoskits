use alloc::{sync::Arc, vec};

use ax_sync::SpinLock;
use axaddrspace::GuestMemoryAccessor;
use axvirtio_common::{
    AddressSpaceMemory, MmioReadOutcome, MmioWriteAction, VirtioDeviceID, VirtioMmioState,
    VirtioQueue, VirtioResult, mmio::transport,
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
                self.core.reset();
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
        let Some(mut queue_lease) = self.state.acquire_queue_processing_lease(queue_index)? else {
            return Ok(BlockDeviceEvent::None);
        };
        let negotiated_features = queue_lease.negotiated_features();
        let pending_head = self.take_pending_head();
        let outcome = self.core.process_queue_with_features(
            queue_lease.queue(),
            memory,
            pending_head,
            negotiated_features,
        )?;
        if let BlockQueueOutcome::Deferred { pending_head, .. } = outcome {
            self.store_pending_head(pending_head);
        }
        drop(queue_lease);
        match outcome {
            BlockQueueOutcome::Idle | BlockQueueOutcome::Completed { notify: false } => {
                Ok(BlockDeviceEvent::None)
            }
            BlockQueueOutcome::Completed { notify: true } => {
                self.trigger_interrupt();
                Ok(BlockDeviceEvent::InterruptPending)
            }
            BlockQueueOutcome::Deferred { notify, .. } => {
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
    extern crate std;

    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::{sync::Barrier, thread};

    use axvirtio_common::{
        GuestMemory, NoGuestMemoryAccessor, VirtioError, constants as vc,
        constants::VIRTQ_DESC_F_NEXT,
    };

    use super::*;

    const DESC_TABLE: usize = 0x100;
    const AVAIL_RING: usize = 0x200;
    const USED_RING: usize = 0x240;
    const BASE: usize = 0x0a00_0000;
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

        fn used_idx(&self) -> u16 {
            u16::from_le_bytes(self.0[USED_RING + 2..USED_RING + 4].try_into().unwrap())
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

    #[derive(Default)]
    struct TestBackend {
        reset: Arc<AtomicBool>,
        writes: Arc<AtomicUsize>,
        ready: Arc<AtomicBool>,
        cancellations: Arc<AtomicUsize>,
    }

    impl BlockBackend for TestBackend {
        fn pending_request_ready(&self) -> bool {
            self.ready.load(Ordering::Relaxed)
        }

        fn cancel_pending_request(&self) {
            self.cancellations.fetch_add(1, Ordering::Relaxed);
        }

        fn reset(&self) {
            self.reset.store(true, Ordering::Relaxed);
        }
        fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            if sector >= 8 {
                return Err(VirtioError::InvalidSector);
            }
            buffer.fill(0x5a);
            Ok(buffer.len())
        }

        fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
            self.writes.fetch_add(1, Ordering::Relaxed);
            if sector == 1 {
                return Err(VirtioError::WouldBlock);
            }
            if sector >= 8 {
                return Err(VirtioError::InvalidSector);
            }
            Ok(buffer.len())
        }

        fn flush(&self) -> VirtioResult<()> {
            Ok(())
        }
    }

    struct BlockingBackend {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        reset_calls: Arc<AtomicUsize>,
    }

    impl BlockBackend for BlockingBackend {
        fn reset(&self) {
            self.reset_calls.fetch_add(1, Ordering::Release);
        }

        fn read(&self, _sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            buffer.fill(0x5a);
            Ok(buffer.len())
        }

        fn write(&self, _sector: u64, _buffer: &[u8]) -> VirtioResult<usize> {
            self.entered.wait();
            self.release.wait();
            Err(VirtioError::WouldBlock)
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
        fixture_with_backend(data_len, sector, TestBackend::default(), false)
    }

    fn fixture_with_backend(
        data_len: u32,
        sector: u64,
        backend: TestBackend,
        read_only: bool,
    ) -> (
        VirtioMmioBlockDevice<TestBackend, NoGuestMemoryAccessor>,
        VirtioQueue<NoGuestMemoryAccessor>,
        TestMemory,
    ) {
        let config = VirtioBlockConfig {
            read_only,
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let device = VirtioMmioBlockDevice::new(
            GuestPhysAddr::from(0x0a00_0000),
            0x200,
            backend,
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

    fn write_register<B: BlockBackend, T: GuestMemoryAccessor + Clone>(
        device: &VirtioMmioBlockDevice<B, T>,
        memory: &mut TestMemory,
        register: usize,
        value: usize,
    ) -> BlockDeviceEvent {
        device
            .mmio_write_with_memory(
                GuestPhysAddr::from(BASE + register),
                AccessWidth::Dword,
                value,
                memory,
            )
            .unwrap()
    }

    fn negotiate_driver_features<B: BlockBackend, T: GuestMemoryAccessor + Clone>(
        device: &VirtioMmioBlockDevice<B, T>,
        memory: &mut TestMemory,
        features: u64,
    ) {
        write_register(device, memory, vc::VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        write_register(
            device,
            memory,
            vc::VIRTIO_MMIO_DRIVER_FEATURES,
            features as usize,
        );
        for status in [
            vc::VIRTIO_STATUS_ACKNOWLEDGE,
            vc::VIRTIO_STATUS_ACKNOWLEDGE | vc::VIRTIO_STATUS_DRIVER,
            vc::VIRTIO_STATUS_ACKNOWLEDGE
                | vc::VIRTIO_STATUS_DRIVER
                | vc::VIRTIO_STATUS_FEATURES_OK,
            vc::VIRTIO_STATUS_ACKNOWLEDGE
                | vc::VIRTIO_STATUS_DRIVER
                | vc::VIRTIO_STATUS_FEATURES_OK
                | vc::VIRTIO_STATUS_DRIVER_OK,
        ] {
            write_register(device, memory, vc::VIRTIO_MMIO_STATUS, status as usize);
        }
    }

    fn configure_queue_from_registers<B: BlockBackend, T: GuestMemoryAccessor + Clone>(
        device: &VirtioMmioBlockDevice<B, T>,
        memory: &mut TestMemory,
    ) {
        for (register, value) in [
            (vc::VIRTIO_MMIO_QUEUE_SEL, 0),
            (vc::VIRTIO_MMIO_QUEUE_NUM, 4),
            (vc::VIRTIO_MMIO_QUEUE_DESC_LOW, DESC_TABLE),
            (vc::VIRTIO_MMIO_QUEUE_AVAIL_LOW, AVAIL_RING),
            (vc::VIRTIO_MMIO_QUEUE_USED_LOW, USED_RING),
            (vc::VIRTIO_MMIO_QUEUE_READY, 1),
        ] {
            write_register(device, memory, register, value);
        }
    }

    fn configure_ready_transport<B: BlockBackend, T: GuestMemoryAccessor + Clone>(
        device: &VirtioMmioBlockDevice<B, T>,
        memory: &mut TestMemory,
        features: u64,
    ) {
        negotiate_driver_features(device, memory, features);
        configure_queue_from_registers(device, memory);
    }

    fn wait_for_reset_status<B: BlockBackend, T: GuestMemoryAccessor + Clone>(
        device: &VirtioMmioBlockDevice<B, T>,
    ) {
        for _ in 0..100_000 {
            if device.get_status() == 0 {
                return;
            }
            thread::yield_now();
        }
        panic!("MMIO reset did not publish status zero");
    }

    #[test]
    fn unaligned_segment_completes_with_ioerr() {
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
        let backend = TestBackend::default();
        let writes = backend.writes.clone();
        let (device, queue, mut memory) = fixture_with_backend(512, 0, backend, true);

        assert_eq!(
            device.core.process_request(&queue, 0, &mut memory),
            Ok(Some(1))
        );
        assert_eq!(memory.0[STATUS], IOERR);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn reset_discards_deferred_request_head() {
        let backend = TestBackend::default();
        let reset_observed = backend.reset.clone();
        let (device, _queue, mut memory) = fixture_with_backend(64, 0, backend, false);
        device.store_pending_head(3);

        let event = device.mmio_write_with_memory(
            GuestPhysAddr::from(0x0a00_0000 + VIRTIO_MMIO_STATUS),
            AccessWidth::Dword,
            0,
            &mut memory,
        );

        assert_eq!(event, Ok(BlockDeviceEvent::Reset));
        assert_eq!(*device.pending_head.lock(), None);
        assert!(reset_observed.load(core::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn completed_request_interrupt_survives_a_later_blocked_request() {
        let backend = TestBackend::default();
        let ready = backend.ready.clone();
        let writes = backend.writes.clone();
        let cancellations = backend.cancellations.clone();
        let (device, _queue, mut memory) = fixture_with_backend(512, 0, backend, false);
        negotiate_driver_features(&device, &mut memory, 0);
        {
            let mut queues = device.state.queues_lock();
            let queue = &mut queues[0];
            queue.set_size(8).unwrap();
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
        }
        memory.set_descriptor(
            3,
            HEADER + 32,
            crate::block::VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            4,
        );
        memory.set_descriptor(4, DATA + 512, 512, VIRTQ_DESC_F_NEXT, 5);
        memory.set_descriptor(5, STATUS + 1, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.0[HEADER + 32..HEADER + 36].copy_from_slice(&VIRTIO_BLK_T_OUT.to_le_bytes());
        memory.0[HEADER + 40..HEADER + 48].copy_from_slice(&1_u64.to_le_bytes());
        memory.0[AVAIL_RING + 2..AVAIL_RING + 4].copy_from_slice(&2_u16.to_le_bytes());
        memory.0[AVAIL_RING + 6..AVAIL_RING + 8].copy_from_slice(&3_u16.to_le_bytes());
        assert_eq!(
            device.handle_queue_notify(0, &mut memory),
            Ok(BlockDeviceEvent::QueuePending(0))
        );
        assert_eq!(*device.pending_head.lock(), Some(3));
        assert_eq!(
            u16::from_le_bytes(memory.0[USED_RING + 2..USED_RING + 4].try_into().unwrap()),
            1
        );
        assert_ne!(
            device.interrupt_status(),
            0,
            "the completed head still requires its EVENT_IDX interrupt"
        );
        assert_eq!(cancellations.load(Ordering::Relaxed), 1);
        let calls = writes.load(Ordering::Relaxed);
        assert_eq!(
            device.process_pending_queue(0, &mut memory),
            Ok(BlockDeviceEvent::QueuePending(0))
        );
        assert_eq!(
            writes.load(Ordering::Relaxed),
            calls,
            "an unready backend must not be retried"
        );
        assert_eq!(cancellations.load(Ordering::Relaxed), 1);

        // A retained head may become malformed before retry. Retire its old
        // backend operation even when descriptor validation fails before I/O.
        ready.store(true, Ordering::Relaxed);
        memory.set_descriptor(3, HEADER + 32, 0, VIRTQ_DESC_F_NEXT, 4);
        device.process_pending_queue(0, &mut memory).unwrap();
        assert_eq!(device.take_pending_head(), None);
        assert_eq!(cancellations.load(Ordering::Relaxed), 2);
        assert_eq!(
            u16::from_le_bytes(memory.0[USED_RING + 2..USED_RING + 4].try_into().unwrap()),
            2
        );
    }

    #[test]
    fn empty_event_idx_queue_rearms_the_next_available_index() {
        let (device, _queue, mut memory) = fixture(64, 0);
        negotiate_driver_features(&device, &mut memory, 0);
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

    #[test]
    fn mmio_queue_uses_only_the_features_negotiated_through_registers() {
        let backend = TestBackend::default();
        let writes = Arc::clone(&backend.writes);
        let (device, _queue, mut memory) = fixture_with_backend(512, 0, backend, false);
        configure_ready_transport(
            &device,
            &mut memory,
            crate::constants::VIRTIO_BLK_F_SIZE_MAX,
        );

        memory.set_descriptor(
            0,
            HEADER,
            crate::block::VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 1024, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, 0);
        memory.0[STATUS] = 0xff;
        memory.0[AVAIL_RING + 2..AVAIL_RING + 4].copy_from_slice(&1_u16.to_le_bytes());
        memory.0[AVAIL_RING + 4..AVAIL_RING + 6].copy_from_slice(&0_u16.to_le_bytes());

        assert_eq!(
            write_register(&device, &mut memory, vc::VIRTIO_MMIO_QUEUE_NOTIFY, 0),
            BlockDeviceEvent::InterruptPending
        );
        assert_eq!(memory.0[STATUS], IOERR);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        assert_eq!(memory.used_idx(), 1);

        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, DATA + 512, 512, VIRTQ_DESC_F_NEXT, 3);
        memory.set_descriptor(3, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.0[STATUS] = 0xff;
        memory.0[AVAIL_RING + 2..AVAIL_RING + 4].copy_from_slice(&2_u16.to_le_bytes());
        memory.0[AVAIL_RING + 6..AVAIL_RING + 8].copy_from_slice(&0_u16.to_le_bytes());

        assert_eq!(
            write_register(&device, &mut memory, vc::VIRTIO_MMIO_QUEUE_NOTIFY, 0),
            BlockDeviceEvent::InterruptPending
        );
        assert_eq!(memory.0[STATUS], 0);
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        assert_eq!(memory.used_idx(), 2);
    }

    #[test]
    fn reset_waits_for_queue_lease_and_clears_deferred_state() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let reset_calls = Arc::new(AtomicUsize::new(0));
        let backend = BlockingBackend {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            reset_calls: Arc::clone(&reset_calls),
        };
        let device = Arc::new(
            VirtioMmioBlockDevice::new(
                GuestPhysAddr::from(BASE),
                0x200,
                backend,
                VirtioBlockConfig {
                    capacity: 8,
                    size_max: 512,
                    seg_max: 1,
                    ..VirtioBlockConfig::default()
                },
                NoGuestMemoryAccessor,
            )
            .unwrap(),
        );
        let mut memory = TestMemory::new();
        configure_ready_transport(&device, &mut memory, 0);
        memory.set_descriptor(
            0,
            HEADER,
            crate::block::VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, 1);
        memory.0[AVAIL_RING + 2..AVAIL_RING + 4].copy_from_slice(&1_u16.to_le_bytes());
        memory.0[AVAIL_RING + 4..AVAIL_RING + 6].copy_from_slice(&0_u16.to_le_bytes());

        let notify_device = Arc::clone(&device);
        let notify = thread::spawn(move || {
            let mut memory = memory;
            let event =
                write_register(&notify_device, &mut memory, vc::VIRTIO_MMIO_QUEUE_NOTIFY, 0);
            (event, memory)
        });
        entered.wait();

        let reset_started = Arc::new(Barrier::new(2));
        let reset_finished = Arc::new(AtomicBool::new(false));
        let reset_device = Arc::clone(&device);
        let reset_started_thread = Arc::clone(&reset_started);
        let reset_finished_thread = Arc::clone(&reset_finished);
        let reset = thread::spawn(move || {
            reset_started_thread.wait();
            let mut memory = TestMemory::new();
            let event = write_register(&reset_device, &mut memory, vc::VIRTIO_MMIO_STATUS, 0);
            reset_finished_thread.store(true, Ordering::Release);
            event
        });
        reset_started.wait();
        wait_for_reset_status(&device);
        assert!(!reset_finished.load(Ordering::Acquire));
        assert_eq!(reset_calls.load(Ordering::Acquire), 0);

        release.wait();
        let (notify_event, memory) = notify.join().expect("queue notify should finish");
        assert_eq!(notify_event, BlockDeviceEvent::QueuePending(0));
        assert_eq!(memory.used_idx(), 0);
        assert_eq!(
            reset.join().expect("reset should finish after queue lease"),
            BlockDeviceEvent::Reset
        );
        assert!(reset_finished.load(Ordering::Acquire));
        assert_eq!(device.get_status(), 0);
        assert_eq!(reset_calls.load(Ordering::Acquire), 1);
        assert_eq!(*device.pending_head.lock(), None);
        assert!(!device.get_queue(0).unwrap().is_valid());
    }
}
