//! Thin VirtIO PCI adapter for the shared block request core.

use core::mem::size_of;

use axdevice_base::{AccessWidth, DeviceError, DeviceResult};
use axvirtio_common::{
    GuestMemory, NoGuestMemoryAccessor, VirtioDeviceID, VirtioQueue,
    constants::VIRTIO_F_VERSION_1,
    map_virtio_error,
    pci::{QueueNotifyOutcome, VirtioDeviceCore},
};

use crate::{
    BlockBackend, BlockQueueOutcome, VirtioBlockConfig, VirtioBlockRequestCore,
    constants::{VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_BLK_F_SEG_MAX, VIRTIO_BLK_F_SIZE_MAX},
};

const QUEUE_SIZE_MAX: u16 = 128;
const DEVICE_CONFIG_SIZE: usize = 16;

/// VirtIO Block device core adapter for the common modern PCI transport.
///
/// PCI topology, BAR placement, interrupt routing, and guest-memory grants
/// remain owned by the AxVM endpoint layer. This type only exposes the block
/// policy and translates queue notifications to the common VirtIO contract.
pub struct VirtioBlockPciAdapter<B: BlockBackend> {
    core: VirtioBlockRequestCore<B>,
}

impl<B: BlockBackend> VirtioBlockPciAdapter<B> {
    /// Creates a PCI block adapter from a backend and block policy.
    pub const fn new(backend: B, config: VirtioBlockConfig) -> Self {
        Self {
            core: VirtioBlockRequestCore::new(backend, config),
        }
    }
}

impl<B: BlockBackend> VirtioDeviceCore for VirtioBlockPciAdapter<B> {
    fn device_type(&self) -> VirtioDeviceID {
        VirtioDeviceID::Block
    }

    fn device_features(&self) -> u64 {
        let mut features = VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_SIZE_MAX | VIRTIO_BLK_F_SEG_MAX;
        if self.core.config().read_only {
            features |= VIRTIO_BLK_F_RO;
        }
        if self.core.config().flush_supported {
            features |= VIRTIO_BLK_F_FLUSH;
        }
        features
    }

    fn queue_num_max(&self) -> u16 {
        1
    }

    fn queue_size_max(&self) -> u16 {
        QUEUE_SIZE_MAX
    }

    fn device_config_size(&self) -> u32 {
        DEVICE_CONFIG_SIZE as u32
    }

    fn read_device_config(&self, offset: u64, width: AccessWidth) -> DeviceResult<u64> {
        let range = device_config_range(offset, width)?;
        let config = encode_device_config(self.core.config());
        let mut value = [0u8; size_of::<u64>()];
        value[..width.size()].copy_from_slice(&config[range]);
        Ok(u64::from_le_bytes(value))
    }

    fn write_device_config(&self, offset: u64, width: AccessWidth, _value: u64) -> DeviceResult {
        device_config_range(offset, width)?;
        Err(DeviceError::ReadOnly)
    }

    fn notify_queue(
        &self,
        queue: &mut VirtioQueue<NoGuestMemoryAccessor>,
        memory: &mut dyn GuestMemory,
    ) -> DeviceResult<QueueNotifyOutcome> {
        self.core
            .process_queue(queue, memory, None)
            .map(|outcome| match outcome {
                BlockQueueOutcome::Idle => QueueNotifyOutcome::Idle,
                BlockQueueOutcome::Completed { notify } => QueueNotifyOutcome::Completed { notify },
                BlockQueueOutcome::Deferred { notify, .. } => {
                    QueueNotifyOutcome::Deferred { notify }
                }
            })
            .map_err(|error| map_virtio_error(error, "process VirtIO PCI block queue"))
    }

    fn requires_deferred_processing(&self) -> bool {
        self.core.requires_deferred_processing()
    }
}

fn device_config_range(offset: u64, width: AccessWidth) -> DeviceResult<core::ops::Range<usize>> {
    let end = offset
        .checked_add(width.size() as u64)
        .ok_or(DeviceError::OutOfRange { addr: offset })?;
    if end > DEVICE_CONFIG_SIZE as u64 {
        return Err(DeviceError::OutOfRange { addr: offset });
    }
    Ok(offset as usize..end as usize)
}

fn encode_device_config(config: &VirtioBlockConfig) -> [u8; DEVICE_CONFIG_SIZE] {
    let mut bytes = [0; DEVICE_CONFIG_SIZE];
    bytes[0x00..0x08].copy_from_slice(&config.capacity.to_le_bytes());
    bytes[0x08..0x0c].copy_from_slice(&config.size_max.to_le_bytes());
    bytes[0x0c..0x10].copy_from_slice(&config.seg_max.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{vec, vec::Vec};
    use std::{
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    use axvirtio_common::{
        GuestMemory, NoGuestMemoryAccessor, VIRTIO_STATUS_DEVICE_NEEDS_RESET, VirtioError,
        VirtioQueue,
        pci::{VirtioPciTransport, VirtioPciWriteOutcome},
    };

    use super::*;
    use crate::{VirtioResult, block::VIRTIO_BLK_REQUEST_HEADER_SIZE};

    const DESC_TABLE: u64 = 0x1000;
    const AVAIL_RING: u64 = 0x1200;
    const USED_RING: u64 = 0x1400;
    const HEADER: u64 = 0x1600;
    const DATA: u64 = 0x1700;
    const STATUS: u64 = 0x1a00;
    const READ_DATA: u64 = 0x1b00;
    const VIRTQ_DESC_F_NEXT: u16 = 1;
    const VIRTQ_DESC_F_WRITE: u16 = 2;
    const VIRTIO_BLK_T_IN: u32 = 0;
    const VIRTIO_BLK_T_OUT: u32 = 1;
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    const VIRTIO_BLK_S_OK: u8 = 0;
    const VIRTIO_BLK_S_IOERR: u8 = 1;

    struct TestMemory {
        bytes: Vec<u8>,
        reads: Vec<(u64, usize)>,
        writes: Vec<(u64, usize)>,
    }

    impl TestMemory {
        fn new() -> Self {
            Self {
                bytes: vec![0; 0x2000],
                reads: Vec::new(),
                writes: Vec::new(),
            }
        }

        fn set_descriptor(&mut self, index: usize, addr: u64, len: u32, flags: u16, next: u16) {
            let offset = DESC_TABLE as usize + index * 16;
            self.bytes[offset..offset + 8].copy_from_slice(&addr.to_le_bytes());
            self.bytes[offset + 8..offset + 12].copy_from_slice(&len.to_le_bytes());
            self.bytes[offset + 12..offset + 14].copy_from_slice(&flags.to_le_bytes());
            self.bytes[offset + 14..offset + 16].copy_from_slice(&next.to_le_bytes());
        }

        fn set_available_head(&mut self, head: u16) {
            self.set_available_head_at(1, head);
        }

        fn set_available_head_at(&mut self, index: u16, head: u16) {
            let offset = AVAIL_RING as usize;
            self.bytes[offset..offset + 2].copy_from_slice(&0u16.to_le_bytes());
            self.bytes[offset + 2..offset + 4].copy_from_slice(&index.to_le_bytes());
            let head_offset = offset + 4 + (usize::from(index) - 1) * 2;
            self.bytes[head_offset..head_offset + 2].copy_from_slice(&head.to_le_bytes());
        }

        fn set_header(&mut self, request_type: u32, sector: u64) {
            let offset = HEADER as usize;
            self.bytes[offset..offset + 4].copy_from_slice(&request_type.to_le_bytes());
            self.bytes[offset + 8..offset + 16].copy_from_slice(&sector.to_le_bytes());
        }

        fn used_idx(&self) -> u16 {
            let offset = USED_RING as usize + 2;
            u16::from_le_bytes(self.bytes[offset..offset + 2].try_into().unwrap())
        }

        fn used_element(&self, index: usize) -> (u32, u32) {
            let offset = USED_RING as usize + 4 + index * 8;
            let head = u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap());
            let len = u32::from_le_bytes(self.bytes[offset + 4..offset + 8].try_into().unwrap());
            (head, len)
        }
    }

    impl GuestMemory for TestMemory {
        fn read(
            &mut self,
            guest_addr: axvm_types::GuestPhysAddr,
            data: &mut [u8],
        ) -> VirtioResult<()> {
            let start = guest_addr.as_usize();
            self.reads.push((guest_addr.as_usize() as u64, data.len()));
            let source = self
                .bytes
                .get(start..start + data.len())
                .ok_or(axvirtio_common::VirtioError::InvalidAddress)?;
            data.copy_from_slice(source);
            Ok(())
        }

        fn write(
            &mut self,
            guest_addr: axvm_types::GuestPhysAddr,
            data: &[u8],
        ) -> VirtioResult<()> {
            let start = guest_addr.as_usize();
            self.writes.push((guest_addr.as_usize() as u64, data.len()));
            let destination = self
                .bytes
                .get_mut(start..start + data.len())
                .ok_or(axvirtio_common::VirtioError::InvalidAddress)?;
            destination.copy_from_slice(data);
            Ok(())
        }
    }

    struct TestBackend {
        sectors: Mutex<Vec<u8>>,
        write_count: Arc<AtomicUsize>,
        flush_count: Arc<AtomicUsize>,
    }

    struct BlockingWriteBackend {
        inner: TestBackend,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        pause_once: AtomicBool,
    }

    impl BlockingWriteBackend {
        fn new(capacity_sectors: usize, entered: Arc<Barrier>, release: Arc<Barrier>) -> Self {
            Self {
                inner: TestBackend::new(capacity_sectors),
                entered,
                release,
                pause_once: AtomicBool::new(true),
            }
        }

        fn write_count(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.inner.write_count)
        }
    }

    impl BlockBackend for BlockingWriteBackend {
        fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            self.inner.read(sector, buffer)
        }

        fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
            if self
                .pause_once
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.entered.wait();
                self.release.wait();
            }
            self.inner.write(sector, buffer)
        }

        fn flush(&self) -> VirtioResult<()> {
            self.inner.flush()
        }
    }

    impl TestBackend {
        fn new(capacity_sectors: usize) -> Self {
            Self {
                sectors: Mutex::new(vec![0x5a; capacity_sectors * 512]),
                write_count: Arc::new(AtomicUsize::new(0)),
                flush_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl BlockBackend for TestBackend {
        fn read(&self, sector: u64, buffer: &mut [u8]) -> VirtioResult<usize> {
            let start = usize::try_from(sector)
                .ok()
                .and_then(|sector| sector.checked_mul(512))
                .ok_or(VirtioError::InvalidAddress)?;
            let end = start
                .checked_add(buffer.len())
                .ok_or(VirtioError::InvalidAddress)?;
            let sectors = self
                .sectors
                .lock()
                .expect("test backend lock should not be poisoned");
            let source = sectors.get(start..end).ok_or(VirtioError::InvalidAddress)?;
            buffer.copy_from_slice(source);
            Ok(buffer.len())
        }

        fn write(&self, sector: u64, buffer: &[u8]) -> VirtioResult<usize> {
            let start = usize::try_from(sector)
                .ok()
                .and_then(|sector| sector.checked_mul(512))
                .ok_or(VirtioError::InvalidAddress)?;
            let end = start
                .checked_add(buffer.len())
                .ok_or(VirtioError::InvalidAddress)?;
            let mut sectors = self
                .sectors
                .lock()
                .expect("test backend lock should not be poisoned");
            let destination = sectors
                .get_mut(start..end)
                .ok_or(VirtioError::InvalidAddress)?;
            destination.copy_from_slice(buffer);
            self.write_count.fetch_add(1, Ordering::Relaxed);
            Ok(buffer.len())
        }

        fn flush(&self) -> VirtioResult<()> {
            self.flush_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn configured_queue() -> VirtioQueue<NoGuestMemoryAccessor> {
        let mut queue = VirtioQueue::new(0, 4, Arc::new(NoGuestMemoryAccessor));
        queue
            .set_desc_table_addr(axvm_types::GuestPhysAddr::from(DESC_TABLE as usize))
            .unwrap();
        queue
            .set_avail_ring_addr(axvm_types::GuestPhysAddr::from(AVAIL_RING as usize))
            .unwrap();
        queue
            .set_used_ring_addr(axvm_types::GuestPhysAddr::from(USED_RING as usize))
            .unwrap();
        queue.set_ready(true);
        queue
    }

    fn configure_transport<B: BlockBackend>(
        backend: B,
    ) -> (
        Arc<VirtioPciTransport<VirtioBlockPciAdapter<B>>>,
        TestMemory,
    ) {
        let transport = Arc::new(
            VirtioPciTransport::try_new(VirtioBlockPciAdapter::new(
                backend,
                VirtioBlockConfig {
                    capacity: 8,
                    size_max: 512,
                    seg_max: 1,
                    ..VirtioBlockConfig::default()
                },
            ))
            .unwrap(),
        );
        let mut memory = TestMemory::new();
        for (offset, width, value) in [
            (0x00, AccessWidth::Dword, 0),
            (0x08, AccessWidth::Dword, 0),
            (0x0c, AccessWidth::Dword, 0),
            (0x14, AccessWidth::Byte, 0x0f),
            (0x16, AccessWidth::Word, 0),
            (0x18, AccessWidth::Word, 4),
            (0x20, AccessWidth::Qword, DESC_TABLE),
            (0x28, AccessWidth::Qword, AVAIL_RING),
            (0x30, AccessWidth::Qword, USED_RING),
            (0x1c, AccessWidth::Word, 1),
        ] {
            transport
                .write_bar_with_dma(offset, width, value, true, &mut memory)
                .unwrap();
        }
        (transport, memory)
    }

    fn prepare_write_request(memory: &mut TestMemory) {
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, 0);
        memory.bytes[DATA as usize..DATA as usize + 512].fill(0xa5);
        memory.set_available_head(0);
    }

    fn wait_for_reset_status<D: VirtioDeviceCore>(transport: &VirtioPciTransport<D>) {
        for _ in 0..100_000 {
            if transport.status() & VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8 != 0 {
                return;
            }
            thread::yield_now();
        }
        panic!("reset did not publish DEVICE_NEEDS_RESET");
    }

    #[test]
    fn reset_waits_for_block_backend_before_guest_visible_completion() {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let backend = BlockingWriteBackend::new(8, Arc::clone(&entered), Arc::clone(&release));
        let write_count = backend.write_count();
        let (transport, memory) = configure_transport(backend);
        let memory = Arc::new(Mutex::new(memory));
        prepare_write_request(&mut memory.lock().unwrap());

        let notify_transport = Arc::clone(&transport);
        let notify_memory = Arc::clone(&memory);
        let notify = thread::spawn(move || {
            notify_transport
                .write_bar_with_dma(
                    0x100,
                    AccessWidth::Word,
                    0,
                    true,
                    &mut *notify_memory.lock().unwrap(),
                )
                .unwrap()
        });
        entered.wait();

        let reset_started = Arc::new(Barrier::new(2));
        let reset_finished = Arc::new(AtomicBool::new(false));
        let reset_transport = Arc::clone(&transport);
        let reset_started_thread = Arc::clone(&reset_started);
        let reset_finished_thread = Arc::clone(&reset_finished);
        let reset = thread::spawn(move || {
            reset_started_thread.wait();
            let result = reset_transport.reset();
            reset_finished_thread.store(true, Ordering::Release);
            result
        });
        reset_started.wait();
        wait_for_reset_status(&transport);
        assert!(!reset_finished.load(Ordering::Acquire));
        assert_eq!(write_count.load(Ordering::Acquire), 0);

        release.wait();
        let notification = notify.join().expect("block notify should finish");
        assert_eq!(write_count.load(Ordering::Acquire), 1);
        let guest = memory.lock().unwrap();
        assert_eq!(guest.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert_eq!(guest.used_idx(), 1);
        drop(guest);
        assert!(!reset_finished.load(Ordering::Acquire));

        drop(notification);
        reset
            .join()
            .expect("reset should finish after block completion")
            .unwrap();
        transport.complete_reset();
        assert_eq!(transport.status(), 0);
        assert_eq!(write_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn reset_waits_after_block_used_status_before_interrupt_publication() {
        let backend = TestBackend::new(8);
        let write_count = Arc::clone(&backend.write_count);
        let (transport, mut memory) = configure_transport(backend);
        prepare_write_request(&mut memory);
        let outcome = transport
            .write_bar_with_dma(0x100, AccessWidth::Word, 0, true, &mut memory)
            .unwrap();
        let notification = match outcome {
            VirtioPciWriteOutcome::QueueNotified(notification) => notification,
            _ => panic!("expected block queue notification"),
        };
        assert_eq!(write_count.load(Ordering::Acquire), 1);
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert_eq!(memory.used_idx(), 1);

        let reset_finished = Arc::new(AtomicBool::new(false));
        let reset_transport = Arc::clone(&transport);
        let reset_finished_thread = Arc::clone(&reset_finished);
        let reset = thread::spawn(move || {
            let result = reset_transport.reset();
            reset_finished_thread.store(true, Ordering::Release);
            result
        });
        wait_for_reset_status(&transport);
        assert!(!reset_finished.load(Ordering::Acquire));

        let published = Arc::new(AtomicBool::new(false));
        let published_callback = Arc::clone(&published);
        notification
            .publish(|_| {
                published_callback.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap();
        assert!(published.load(Ordering::Acquire));
        reset
            .join()
            .expect("reset should finish after publication")
            .unwrap();
        transport.complete_reset();
        assert_eq!(transport.status(), 0);
        assert_eq!(write_count.load(Ordering::Acquire), 1);
        assert_eq!(memory.used_idx(), 1);
    }

    fn run_write_request(
        config: VirtioBlockConfig,
        sector: u64,
        data_len: u32,
        setup: impl FnOnce(&mut TestMemory),
    ) -> (TestMemory, Arc<AtomicUsize>) {
        let backend = TestBackend::new(8);
        let write_count = Arc::clone(&backend.write_count);
        let adapter = VirtioBlockPciAdapter::new(backend, config);
        let mut queue = configured_queue();
        let mut memory = TestMemory::new();
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, data_len, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, sector);
        memory.set_available_head(0);
        setup(&mut memory);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();
        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        (memory, write_count)
    }

    #[test]
    fn advertises_policy_and_encodes_request_limits() {
        let config = VirtioBlockConfig {
            read_only: true,
            flush_supported: false,
            capacity: 0x8877_6655_4433_2211,
            size_max: 0x0bad_f00d,
            seg_max: 0x1020_3040,
            ..VirtioBlockConfig::default()
        };
        let adapter = VirtioBlockPciAdapter::new(TestBackend::new(8), config);
        let expected_features =
            VIRTIO_F_VERSION_1 | VIRTIO_BLK_F_RO | VIRTIO_BLK_F_SIZE_MAX | VIRTIO_BLK_F_SEG_MAX;

        assert_eq!(adapter.device_features(), expected_features);
        assert_eq!(adapter.device_config_size(), 16);
        assert_eq!(
            adapter
                .read_device_config(0x00, AccessWidth::Qword)
                .unwrap(),
            0x8877_6655_4433_2211
        );
        assert_eq!(
            adapter
                .read_device_config(0x08, AccessWidth::Dword)
                .unwrap(),
            0x0bad_f00d
        );
        assert_eq!(
            adapter
                .read_device_config(0x0c, AccessWidth::Dword)
                .unwrap(),
            0x1020_3040
        );
        assert!(matches!(
            adapter.read_device_config(0x0f, AccessWidth::Word),
            Err(DeviceError::OutOfRange { addr: 0x0f })
        ));
        assert!(matches!(
            adapter.write_device_config(0x08, AccessWidth::Dword, 0),
            Err(DeviceError::ReadOnly)
        ));
    }

    #[test]
    fn processes_read_through_shared_request_core() {
        let config = VirtioBlockConfig {
            read_only: true,
            flush_supported: false,
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let adapter = VirtioBlockPciAdapter::new(TestBackend::new(8), config);
        let mut queue = VirtioQueue::new(0, 4, Arc::new(NoGuestMemoryAccessor));
        queue
            .set_desc_table_addr(axvm_types::GuestPhysAddr::from(DESC_TABLE as usize))
            .unwrap();
        queue
            .set_avail_ring_addr(axvm_types::GuestPhysAddr::from(AVAIL_RING as usize))
            .unwrap();
        queue
            .set_used_ring_addr(axvm_types::GuestPhysAddr::from(USED_RING as usize))
            .unwrap();
        queue.set_ready(true);

        let mut memory = TestMemory::new();
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_available_head(0);
        memory.set_header(VIRTIO_BLK_T_IN, 0);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();

        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert_eq!(memory.bytes[DATA as usize], 0x5a);
        assert_eq!(memory.bytes[DATA as usize + 511], 0x5a);
    }

    #[test]
    fn writable_ramdisk_supports_write_read_after_write_and_dma_directions() {
        let backend = TestBackend::new(8);
        let write_count = Arc::clone(&backend.write_count);
        let config = VirtioBlockConfig {
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let adapter = VirtioBlockPciAdapter::new(backend, config);
        let mut queue = VirtioQueue::new(0, 4, Arc::new(NoGuestMemoryAccessor));
        queue
            .set_desc_table_addr(axvm_types::GuestPhysAddr::from(DESC_TABLE as usize))
            .unwrap();
        queue
            .set_avail_ring_addr(axvm_types::GuestPhysAddr::from(AVAIL_RING as usize))
            .unwrap();
        queue
            .set_used_ring_addr(axvm_types::GuestPhysAddr::from(USED_RING as usize))
            .unwrap();
        queue.set_ready(true);

        let mut memory = TestMemory::new();
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, 2);
        memory.bytes[DATA as usize..DATA as usize + 512].fill(0xa5);
        memory.set_available_head(0);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();
        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert_eq!(write_count.load(Ordering::Relaxed), 1);
        assert!(memory.reads.contains(&(DATA, 512)));
        assert_eq!(memory.bytes[DATA as usize], 0xa5);
        assert_eq!(memory.used_idx(), 1);
        assert_eq!(memory.used_element(0), (0, 1));

        memory.set_descriptor(1, READ_DATA, 512, VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, 2);
        memory.bytes[READ_DATA as usize..READ_DATA as usize + 512].fill(0);
        memory.set_header(VIRTIO_BLK_T_IN, 2);
        memory.set_available_head_at(2, 0);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();
        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert!(memory.writes.contains(&(READ_DATA, 512)));
        assert!(
            memory.bytes[READ_DATA as usize..READ_DATA as usize + 512]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
        assert_eq!(memory.used_idx(), 2);
        assert_eq!(memory.used_element(1), (0, 513));
    }

    #[test]
    fn read_only_write_completes_with_ioerr_without_backend_mutation() {
        let backend = TestBackend::new(8);
        let write_count = Arc::clone(&backend.write_count);
        let config = VirtioBlockConfig {
            read_only: true,
            flush_supported: false,
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let adapter = VirtioBlockPciAdapter::new(backend, config);
        let mut queue = VirtioQueue::new(0, 4, Arc::new(NoGuestMemoryAccessor));
        queue
            .set_desc_table_addr(axvm_types::GuestPhysAddr::from(DESC_TABLE as usize))
            .unwrap();
        queue
            .set_avail_ring_addr(axvm_types::GuestPhysAddr::from(AVAIL_RING as usize))
            .unwrap();
        queue
            .set_used_ring_addr(axvm_types::GuestPhysAddr::from(USED_RING as usize))
            .unwrap();
        queue.set_ready(true);

        let mut memory = TestMemory::new();
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
        memory.set_descriptor(2, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_OUT, 0);
        memory.set_available_head(0);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();
        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_IOERR);
        assert_eq!(write_count.load(Ordering::Relaxed), 0);
        assert!(memory.writes.contains(&(STATUS, 1)));
    }

    #[test]
    fn supported_flush_dispatches_to_backend_and_completes() {
        let backend = TestBackend::new(8);
        let flush_count = Arc::clone(&backend.flush_count);
        let config = VirtioBlockConfig {
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let adapter = VirtioBlockPciAdapter::new(backend, config);
        assert_ne!(adapter.device_features() & VIRTIO_BLK_F_FLUSH, 0);

        let mut queue = VirtioQueue::new(0, 4, Arc::new(NoGuestMemoryAccessor));
        queue
            .set_desc_table_addr(axvm_types::GuestPhysAddr::from(DESC_TABLE as usize))
            .unwrap();
        queue
            .set_avail_ring_addr(axvm_types::GuestPhysAddr::from(AVAIL_RING as usize))
            .unwrap();
        queue
            .set_used_ring_addr(axvm_types::GuestPhysAddr::from(USED_RING as usize))
            .unwrap();
        queue.set_ready(true);

        let mut memory = TestMemory::new();
        memory.set_descriptor(
            0,
            HEADER,
            VIRTIO_BLK_REQUEST_HEADER_SIZE,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        memory.set_descriptor(1, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        memory.set_header(VIRTIO_BLK_T_FLUSH, 0);
        memory.set_available_head(0);

        let outcome = adapter.notify_queue(&mut queue, &mut memory).unwrap();
        assert!(matches!(
            outcome,
            QueueNotifyOutcome::Completed { notify: true }
        ));
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_OK);
        assert_eq!(flush_count.load(Ordering::Relaxed), 1);
        assert!(memory.writes.contains(&(STATUS, 1)));
        assert_eq!(memory.used_idx(), 1);
        assert_eq!(memory.used_element(0), (0, 1));
    }

    #[test]
    fn rejects_header_and_status_direction_errors_before_backend_access() {
        for (header_flags, status_flags) in [
            (VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE, VIRTQ_DESC_F_WRITE),
            (VIRTQ_DESC_F_NEXT, 0),
        ] {
            let config = VirtioBlockConfig {
                capacity: 8,
                size_max: 512,
                seg_max: 1,
                ..VirtioBlockConfig::default()
            };
            let (memory, write_count) = run_write_request(config, 0, 512, |memory| {
                memory.set_descriptor(0, HEADER, 16, header_flags, 1);
                memory.set_descriptor(2, STATUS, 1, status_flags, 0);
                memory.bytes[STATUS as usize] = 0xff;
            });

            assert_eq!(memory.bytes[STATUS as usize], 0xff);
            assert_eq!(write_count.load(Ordering::Relaxed), 0);
            assert_eq!(memory.used_idx(), 1);
            assert_eq!(memory.used_element(0), (0, 0));
        }
    }

    #[test]
    fn rejects_sector_end_length_and_segment_limits_before_data_copy() {
        let config = VirtioBlockConfig {
            capacity: 8,
            size_max: 512,
            seg_max: 1,
            ..VirtioBlockConfig::default()
        };
        let (memory, write_count) = run_write_request(config.clone(), u64::MAX, 512, |_| {});
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_IOERR);
        assert_eq!(write_count.load(Ordering::Relaxed), 0);
        assert!(!memory.reads.contains(&(DATA, 512)));

        let (memory, write_count) = run_write_request(config.clone(), u64::MAX / 512, 512, |_| {});
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_IOERR);
        assert_eq!(write_count.load(Ordering::Relaxed), 0);
        assert!(!memory.reads.contains(&(DATA, 512)));

        let (memory, write_count) = run_write_request(
            VirtioBlockConfig {
                size_max: 511,
                ..config.clone()
            },
            0,
            512,
            |_| {},
        );
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_IOERR);
        assert_eq!(write_count.load(Ordering::Relaxed), 0);
        assert!(!memory.reads.contains(&(DATA, 512)));

        let (memory, write_count) = run_write_request(config.clone(), 0, 512, |memory| {
            memory.set_descriptor(1, DATA, 512, VIRTQ_DESC_F_NEXT, 2);
            memory.set_descriptor(2, READ_DATA, 512, VIRTQ_DESC_F_NEXT, 3);
            memory.set_descriptor(3, STATUS, 1, VIRTQ_DESC_F_WRITE, 0);
        });
        assert_eq!(memory.bytes[STATUS as usize], VIRTIO_BLK_S_IOERR);
        assert_eq!(write_count.load(Ordering::Relaxed), 0);
        assert!(!memory.reads.contains(&(DATA, 512)));
        assert!(!memory.reads.contains(&(READ_DATA, 512)));
    }
}
