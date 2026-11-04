use core::{
    mem::size_of,
    num::NonZeroUsize,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU16, AtomicUsize, Ordering},
};
use std::{
    alloc::{alloc_zeroed, dealloc},
    sync::{Arc, Mutex},
};

use dma_api::{
    DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
    DmaDomainId, DmaMapHandle, DmaOp,
};
use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, PhysAddr,
    device::common::Feature,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};
use virtio_sound::{
    PcmBuffer, PcmCapabilities, PcmCard, PcmCardState, PcmCompletion, PcmConfig, PcmDirection,
    PcmError, PcmServiceBudget, PcmStream, SampleFormat, VirtioSound,
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

const QUEUE_SIZE: usize = 32;
const CONTROL_QUEUE: u16 = 0;
const RX_QUEUE: u16 = 3;
const S_OK: u32 = 0x8000;
const R_PCM_INFO: u32 = 0x0100;
const PERIOD_FRAMES: u32 = 480;
const PERIOD_COUNT: u16 = 8;
const PERIOD_BYTES: usize = PERIOD_FRAMES as usize * 4;
const WRITE: u16 = 2;
const NEXT: u16 = 1;

static TEST_LOCK: Mutex<()> = Mutex::new(());
static HAL_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static HAL_DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static PCM_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static PCM_DEALLOCS: AtomicUsize = AtomicUsize::new(0);

fn reset_counts() {
    HAL_ALLOCS.store(0, Ordering::SeqCst);
    HAL_DEALLOCS.store(0, Ordering::SeqCst);
    PCM_ALLOCS.store(0, Ordering::SeqCst);
    PCM_DEALLOCS.store(0, Ordering::SeqCst);
}

struct TestHal;

unsafe impl Hal for TestHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let layout = core::alloc::Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: the test layout is non-zero sized and page aligned.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap();
        HAL_ALLOCS.fetch_add(1, Ordering::SeqCst);
        (ptr.as_ptr() as PhysAddr, ptr)
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let layout = core::alloc::Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: this matches the allocation made by dma_alloc.
        unsafe { dealloc(vaddr.as_ptr(), layout) };
        HAL_DEALLOCS.fetch_add(1, Ordering::SeqCst);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new(paddr as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        buffer.as_ptr() as *mut u8 as PhysAddr
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}

struct TestDmaOp;

static TEST_DMA: TestDmaOp = TestDmaOp;

impl TestDmaOp {
    unsafe fn alloc(
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        let addr = ptr.as_ptr() as u64;
        if addr.checked_add(layout.size() as u64)? > constraints.addr_mask {
            // SAFETY: the allocation has not been published.
            unsafe { dealloc(ptr.as_ptr(), layout) };
            return None;
        }
        PCM_ALLOCS.fetch_add(1, Ordering::SeqCst);
        // SAFETY: the CPU pointer and identity device address describe the
        // same live allocation.
        Some(unsafe { DmaAllocHandle::new(ptr, ptr, addr.into(), layout) })
    }
}

impl DmaOp for TestDmaOp {
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::alloc(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: the caller returns an allocation made by this backend.
        unsafe { dealloc(handle.allocation_ptr().as_ptr(), handle.layout()) };
        PCM_DEALLOCS.fetch_add(1, Ordering::SeqCst);
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { Self::alloc(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), dma_api::DmaError> {
        // SAFETY: the caller returns an allocation made by this backend.
        unsafe { dealloc(handle.allocation_ptr().as_ptr(), handle.layout()) };
        PCM_DEALLOCS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, dma_api::DmaError> {
        Err(dma_api::DmaError::MappingFailed)
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

#[derive(Clone, Copy, Debug, Default)]
struct QueueState {
    size: u32,
    descriptors: PhysAddr,
    driver_area: PhysAddr,
    device_area: PhysAddr,
}

#[derive(Clone, Copy, Debug, Default)]
enum ControlMode {
    #[default]
    Normal,
    Drop,
}

#[derive(Clone, Copy, Debug, Default)]
enum TxMode {
    #[default]
    Normal,
    BadLength,
}

#[derive(Clone, Copy, Debug, Default)]
enum RxMode {
    #[default]
    Normal,
    ShortFrame(usize),
    Drop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RxSubmission {
    stream_id: u32,
    input_count: usize,
    output_count: usize,
    data_len: usize,
    status_len: usize,
}

struct FakeState {
    status: DeviceStatus,
    sticky_reset: bool,
    queues: [QueueState; 4],
    config: [u8; 12],
    control_mode: ControlMode,
    tx_mode: TxMode,
    rx_mode: RxMode,
    period_bytes: usize,
    rx_submissions: usize,
    last_rx: Option<RxSubmission>,
}

impl FakeState {
    fn new() -> Self {
        let mut config = [0_u8; 12];
        config[4..8].copy_from_slice(&2_u32.to_le_bytes());
        Self {
            status: DeviceStatus::empty(),
            sticky_reset: false,
            queues: [QueueState::default(); 4],
            config,
            control_mode: ControlMode::Normal,
            tx_mode: TxMode::Normal,
            rx_mode: RxMode::Normal,
            period_bytes: PERIOD_BYTES,
            rx_submissions: 0,
            last_rx: None,
        }
    }

    fn set_status(&mut self, status: DeviceStatus) {
        if status.is_empty() && self.sticky_reset {
            self.status = DeviceStatus::ACKNOWLEDGE;
        } else {
            self.status = status;
        }
    }

    fn notify(&mut self, queue: u16) {
        if queue >= 4 || queue == 1 {
            return;
        }
        let queue_state = self.queues[usize::from(queue)];
        if queue_state.size == 0 || queue_state.descriptors == 0 {
            return;
        }

        // SAFETY: queue addresses are the live TestHal allocations registered
        // by VirtQueue. The test driver and this fake device execute serially;
        // ring publication uses the same atomics as VirtQueue.
        unsafe {
            let avail = &*(queue_state.driver_area as *const AvailRing);
            let used = &mut *(queue_state.device_area as *mut UsedRing);
            let used_index = used.idx.load(Ordering::Acquire);
            if avail.idx.load(Ordering::Acquire) == used_index {
                return;
            }
            let slot = usize::from(used_index) & (QUEUE_SIZE - 1);
            let head = avail.ring[slot];
            let descriptors = queue_state.descriptors as *const Descriptor;

            let mut index = head;
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            loop {
                let descriptor = *descriptors.add(usize::from(index));
                if descriptor.flags & 4 != 0 {
                    return;
                }
                if descriptor.flags & WRITE != 0 {
                    outputs.push((descriptor.addr, descriptor.len));
                } else {
                    inputs.push((descriptor.addr, descriptor.len));
                }
                if descriptor.flags & NEXT == 0 {
                    break;
                }
                index = descriptor.next;
            }

            let used_len = match queue {
                CONTROL_QUEUE => {
                    if matches!(self.control_mode, ControlMode::Drop) {
                        return;
                    }
                    let Some((request_addr, request_len)) = inputs.first().copied() else {
                        return;
                    };
                    if request_len < size_of::<u32>() as u32 {
                        return;
                    }
                    let code = ptr::read_unaligned(request_addr as *const u32);
                    if code == 0x0101 && request_len >= 24 {
                        let period = ptr::read_unaligned((request_addr + 12) as *const u32);
                        self.period_bytes = period as usize;
                    }
                    let response = self.control_response(code);
                    let written = write_outputs(&outputs, &response);
                    if written != response.len() {
                        return;
                    }
                    written
                }
                2 => {
                    let mut status = [0_u8; 8];
                    status[..4].copy_from_slice(&S_OK.to_le_bytes());
                    let _ = write_outputs(&outputs, &status);
                    if matches!(self.tx_mode, TxMode::BadLength) {
                        7
                    } else {
                        status.len()
                    }
                }
                RX_QUEUE => {
                    let stream_id = inputs.first().map_or(u32::MAX, |(address, length)| {
                        if *length >= size_of::<u32>() as u32 {
                            u32::from_le(ptr::read_unaligned(*address as *const u32))
                        } else {
                            u32::MAX
                        }
                    });
                    self.rx_submissions += 1;
                    self.last_rx = Some(RxSubmission {
                        stream_id,
                        input_count: inputs.len(),
                        output_count: outputs.len(),
                        data_len: outputs.first().map_or(0, |(_, length)| *length as usize),
                        status_len: outputs.get(1).map_or(0, |(_, length)| *length as usize),
                    });
                    if matches!(self.rx_mode, RxMode::Drop) {
                        return;
                    }
                    let data_len = match self.rx_mode {
                        RxMode::Normal => self.period_bytes,
                        RxMode::ShortFrame(len) => len,
                        RxMode::Drop => unreachable!("drop mode returned above"),
                    };
                    let data = vec![0x5a_u8; data_len];
                    if outputs.is_empty() {
                        return;
                    }
                    // The outer block proves the first output descriptor is
                    // the live PCM buffer.
                    ptr::copy_nonoverlapping(data.as_ptr(), outputs[0].0 as *mut u8, data.len());
                    let mut status = [0_u8; 8];
                    status[..4].copy_from_slice(&S_OK.to_le_bytes());
                    if outputs.len() < 2 {
                        return;
                    }
                    // The outer block proves the second output descriptor is
                    // the live PCM status.
                    ptr::copy_nonoverlapping(
                        status.as_ptr(),
                        outputs[1].0 as *mut u8,
                        status.len(),
                    );
                    data_len + status.len()
                }
                _ => return,
            };

            used.ring[slot].id = u32::from(head);
            used.ring[slot].len = used_len as u32;
            used.idx
                .store(used_index.wrapping_add(1), Ordering::Release);
        }
    }

    fn control_response(&self, code: u32) -> Vec<u8> {
        match code {
            R_PCM_INFO => {
                let mut response = vec![0_u8; 4 + 2 * 32];
                response[..4].copy_from_slice(&S_OK.to_le_bytes());
                write_pcm_info(&mut response, 0, 0);
                write_pcm_info(&mut response, 1, 1);
                response
            }
            0x0101 => {
                let mut response = vec![0_u8; 4];
                response.copy_from_slice(&S_OK.to_le_bytes());
                response
            }
            0x0102..=0x0105 => S_OK.to_le_bytes().to_vec(),
            _ => 0x8001_u32.to_le_bytes().to_vec(),
        }
    }
}

fn write_pcm_info(response: &mut [u8], index: usize, direction: u8) {
    let offset = 4 + index * 32;
    response[offset + 8..offset + 16].copy_from_slice(&(1_u64 << 5).to_le_bytes());
    response[offset + 16..offset + 24].copy_from_slice(&(1_u64 << 7).to_le_bytes());
    response[offset + 24] = direction;
    response[offset + 25] = 2;
    response[offset + 26] = 2;
}

fn write_outputs(outputs: &[(PhysAddr, u32)], bytes: &[u8]) -> usize {
    let mut offset = 0;
    for (address, length) in outputs {
        let remaining = bytes.len() - offset;
        let count = remaining.min(*length as usize);
        if count == 0 {
            break;
        }
        // SAFETY: descriptors point at live TestHal DMA buffers.
        unsafe {
            ptr::copy_nonoverlapping(bytes[offset..].as_ptr(), *address as *mut u8, count);
        }
        offset += count;
        if offset == bytes.len() {
            break;
        }
    }
    offset
}

#[repr(C)]
struct AvailRing {
    flags: AtomicU16,
    idx: AtomicU16,
    ring: [u16; QUEUE_SIZE],
    used_event: AtomicU16,
}

#[repr(C)]
struct UsedRing {
    flags: AtomicU16,
    idx: AtomicU16,
    ring: [UsedElem; QUEUE_SIZE],
    avail_event: AtomicU16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Descriptor {
    addr: PhysAddr,
    len: u32,
    flags: u16,
    next: u16,
}

struct FakeTransport {
    state: Arc<Mutex<FakeState>>,
}

impl Transport for FakeTransport {
    fn device_type(&self) -> DeviceType {
        DeviceType::Sound
    }

    fn read_device_features(&mut self) -> u64 {
        Feature::VERSION_1.bits()
    }

    fn write_driver_features(&mut self, _driver_features: u64) {}

    fn max_queue_size(&mut self, _queue: u16) -> u32 {
        QUEUE_SIZE as u32
    }

    fn notify(&mut self, queue: u16) {
        self.state.lock().unwrap().notify(queue);
    }

    fn get_status(&self) -> DeviceStatus {
        self.state.lock().unwrap().status
    }

    fn set_status(&mut self, status: DeviceStatus) {
        self.state.lock().unwrap().set_status(status);
    }

    fn set_guest_page_size(&mut self, _guest_page_size: u32) {}

    fn requires_legacy_layout(&self) -> bool {
        false
    }

    fn queue_set(
        &mut self,
        queue: u16,
        size: u32,
        descriptors: PhysAddr,
        driver_area: PhysAddr,
        device_area: PhysAddr,
    ) {
        let mut state = self.state.lock().unwrap();
        state.queues[usize::from(queue)] = QueueState {
            size,
            descriptors,
            driver_area,
            device_area,
        };
    }

    fn queue_unset(&mut self, queue: u16) {
        self.state.lock().unwrap().queues[usize::from(queue)] = QueueState::default();
    }

    fn queue_used(&mut self, queue: u16) -> bool {
        self.state.lock().unwrap().queues[usize::from(queue)].descriptors != 0
    }

    fn ack_interrupt(&mut self) -> InterruptStatus {
        InterruptStatus::empty()
    }

    fn read_config_generation(&self) -> u32 {
        0
    }

    fn read_config_space<T: FromBytes + IntoBytes>(
        &self,
        offset: usize,
    ) -> Result<T, virtio_drivers::Error> {
        let end = offset
            .checked_add(size_of::<T>())
            .ok_or(virtio_drivers::Error::ConfigSpaceTooSmall)?;
        let state = self.state.lock().unwrap();
        let bytes = state
            .config
            .get(offset..end)
            .ok_or(virtio_drivers::Error::ConfigSpaceTooSmall)?;
        T::read_from_bytes(bytes).map_err(|_| virtio_drivers::Error::ConfigSpaceTooSmall)
    }

    fn write_config_space<T: Immutable + IntoBytes>(
        &mut self,
        offset: usize,
        value: T,
    ) -> Result<(), virtio_drivers::Error> {
        let end = offset
            .checked_add(value.as_bytes().len())
            .ok_or(virtio_drivers::Error::ConfigSpaceTooSmall)?;
        let mut state = self.state.lock().unwrap();
        state
            .config
            .get_mut(offset..end)
            .ok_or(virtio_drivers::Error::ConfigSpaceTooSmall)?
            .copy_from_slice(value.as_bytes());
        Ok(())
    }
}

type TestCard = VirtioSound<TestHal, FakeTransport>;

fn new_card() -> (TestCard, Arc<Mutex<FakeState>>) {
    let state = Arc::new(Mutex::new(FakeState::new()));
    let transport = FakeTransport {
        state: state.clone(),
    };
    let info = DmaDeviceInfo::new(
        DmaDomainId::Direct,
        DmaCoherency::Coherent,
        DmaConstraints::new(u64::MAX),
    );
    let dma = DeviceDma::new(info, &TEST_DMA);
    // SAFETY: TestHal and DeviceDma use the same identity address domain.
    let card = unsafe { VirtioSound::new(transport, dma, 0, 1_000_000) }.unwrap();
    (card, state)
}

fn config() -> PcmConfig {
    PcmConfig {
        sample_format: SampleFormat::S16Le,
        sample_rate_hz: 48_000,
        channels: 2,
        period_frames: PERIOD_FRAMES,
        period_count: PERIOD_COUNT,
    }
}

fn take_capture_buffers(card: &mut TestCard) -> Vec<PcmBuffer> {
    let mut buffers = Vec::new();
    while let Some(completion) = card.take_completion() {
        match completion {
            PcmCompletion::Capture {
                buffer,
                frames,
                latency_bytes,
                ..
            } => {
                assert_eq!(frames, PERIOD_FRAMES);
                assert_eq!(latency_bytes, 0);
                assert_eq!(buffer.as_slice().len(), PERIOD_BYTES);
                assert!(buffer.as_slice().iter().all(|byte| *byte == 0x5a));
                buffers.push(buffer);
            }
            PcmCompletion::Fault(error) => panic!("unexpected fault: {error:?}"),
            _ => panic!("unexpected completion while receiving capture"),
        }
    }
    buffers
}

fn assert_rx_submission(state: &Arc<Mutex<FakeState>>, expected: usize) {
    let (submissions, last_rx) = {
        let state = state.lock().unwrap();
        (state.rx_submissions, state.last_rx)
    };
    assert_eq!(submissions, expected);
    assert_eq!(
        last_rx,
        Some(RxSubmission {
            stream_id: 1,
            input_count: 1,
            output_count: 2,
            data_len: PERIOD_BYTES,
            status_len: 8,
        })
    );
}

fn stop_and_release(card: &mut TestCard, stream: PcmStream, now_ns: u64) {
    let deadline_ns = now_ns + 1_000;
    let _ = card.stop(stream, now_ns, deadline_ns).unwrap();
    let (result, other) = control_round(
        card,
        now_ns + 1,
        PcmServiceBudget::new(4, 0, PERIOD_COUNT as u8, 0),
    );
    result.unwrap();
    assert!(other.is_empty());

    let _ = card.release(stream, now_ns + 2, deadline_ns).unwrap();
    let (result, other) = control_round(
        card,
        now_ns + 3,
        PcmServiceBudget::new(4, 0, PERIOD_COUNT as u8, 0),
    );
    result.unwrap();
    assert!(other.is_empty());
}

fn discover(card: &mut TestCard) -> PcmCapabilities {
    for _ in 0..8 {
        let _ = card.service(1, PcmServiceBudget::uniform(8));
        while let Some(completion) = card.take_completion() {
            if matches!(completion, PcmCompletion::Capabilities) {
                return *card.capabilities().unwrap();
            }
        }
    }
    panic!("device discovery did not complete");
}

fn control_round(
    card: &mut TestCard,
    now_ns: u64,
    budget: PcmServiceBudget,
) -> (Result<(), PcmError>, Vec<PcmCompletion>) {
    let mut other = Vec::new();
    for _ in 0..8 {
        let _ = card.service(now_ns, budget);
        let mut control_result = None;
        while let Some(completion) = card.take_completion() {
            if let PcmCompletion::Control { result, .. } = completion {
                control_result = Some(result);
            } else {
                other.push(completion);
            }
        }
        if let Some(result) = control_result {
            return (result, other);
        }
    }
    panic!("control request did not complete");
}

fn shutdown(card: &mut TestCard, now_ns: u64) {
    card.begin_shutdown(now_ns, now_ns + 1_000).unwrap();
    let report = card.service(now_ns + 1, PcmServiceBudget::uniform(8));
    assert_eq!(report.state, PcmCardState::Stopped);
    assert!(report.transport_stopped);
}

#[test]
fn discover_tx_rx_and_confirmed_shutdown() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_counts();
    let (mut card, state) = new_card();
    let capabilities = discover(&mut card);
    assert!(capabilities.playback.is_some());
    assert!(capabilities.capture.is_some());

    let playback_stream = card.open(PcmDirection::Playback).unwrap();
    let capture_stream = card.open(PcmDirection::Capture).unwrap();
    let playback_token = card
        .configure(playback_stream, config(), 10, 1_000)
        .unwrap();
    assert_eq!(
        playback_token.operation(),
        virtio_sound::PcmControlOp::Configure
    );
    let (result, _) = control_round(&mut card, 11, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let capture_token = card.configure(capture_stream, config(), 12, 1_000).unwrap();
    assert_eq!(
        capture_token.operation(),
        virtio_sound::PcmControlOp::Configure
    );
    let (result, _) = control_round(&mut card, 13, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();

    let _ = card.prepare(playback_stream, 14, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 15, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let _ = card.prepare(capture_stream, 16, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 17, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let _ = card.start(playback_stream, 18, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 19, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let _ = card.start(capture_stream, 20, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 21, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();

    let _ = card
        .write_playback(playback_stream, &vec![0x33; PERIOD_BYTES], 1_000)
        .unwrap();
    let report = card.service(22, PcmServiceBudget::new(0, 1, 0, 0));
    assert_eq!(report.error, None);
    let mut saw_playback = false;
    while let Some(completion) = card.take_completion() {
        match completion {
            PcmCompletion::Playback { frames, .. } => {
                assert_eq!(frames, PERIOD_FRAMES);
                saw_playback = true;
            }
            PcmCompletion::Fault(error) => panic!("unexpected fault: {error:?}"),
            _ => panic!("unexpected completion while receiving playback"),
        }
    }
    assert!(saw_playback);

    let report = card.service(23, PcmServiceBudget::new(0, 0, PERIOD_COUNT as u8, 0));
    assert_eq!(report.error, None);
    let initial_captures = take_capture_buffers(&mut card);
    assert_eq!(initial_captures.len(), usize::from(PERIOD_COUNT));

    let initial_submissions = state.lock().unwrap().rx_submissions;
    for (offset, buffer) in initial_captures.into_iter().enumerate() {
        card.recycle_capture(capture_stream, buffer, 0).unwrap();
        assert_rx_submission(&state, initial_submissions + offset + 1);
    }

    let report = card.service(
        2_000_000_000,
        PcmServiceBudget::new(0, 0, PERIOD_COUNT as u8, 0),
    );
    assert_eq!(report.error, None);
    let recycled_captures = take_capture_buffers(&mut card);
    assert_eq!(recycled_captures.len(), usize::from(PERIOD_COUNT));
    let recycled_submissions = {
        let state = state.lock().unwrap();
        state.rx_submissions
    };
    assert_eq!(
        recycled_submissions,
        initial_submissions + usize::from(PERIOD_COUNT)
    );
    drop(recycled_captures);

    stop_and_release(&mut card, playback_stream, 4_000_000_000);
    stop_and_release(&mut card, capture_stream, 5_000_000_000);
    assert_eq!(card.state(), PcmCardState::Ready);
    shutdown(&mut card, 6_000_000_000);
    assert_eq!(
        HAL_DEALLOCS.load(Ordering::SeqCst),
        HAL_ALLOCS.load(Ordering::SeqCst)
    );
    assert_eq!(
        PCM_DEALLOCS.load(Ordering::SeqCst),
        PCM_ALLOCS.load(Ordering::SeqCst)
    );
    let final_status = {
        let state = state.lock().unwrap();
        state.status
    };
    assert_eq!(final_status, DeviceStatus::empty());
}

#[test]
fn nonzero_timeouts_keep_dma_until_reset_confirmed() {
    let _guard = TEST_LOCK.lock().unwrap();
    {
        reset_counts();
        let (mut card, state) = new_card();
        let capabilities = discover(&mut card);
        assert!(capabilities.playback.is_some());
        let stream = card.open(PcmDirection::Playback).unwrap();

        {
            let mut state = state.lock().unwrap();
            state.control_mode = ControlMode::Drop;
            state.sticky_reset = true;
        }
        let _ = card.configure(stream, config(), 10, 20).unwrap();
        let deallocations = PCM_DEALLOCS.load(Ordering::SeqCst);
        let hal_deallocations = HAL_DEALLOCS.load(Ordering::SeqCst);
        let (result, _) = control_round(&mut card, 21, PcmServiceBudget::new(4, 0, 0, 0));
        assert_eq!(result, Err(PcmError::Timeout));
        assert_eq!(card.state(), PcmCardState::Resetting);
        assert_eq!(PCM_DEALLOCS.load(Ordering::SeqCst), deallocations);
        assert_eq!(HAL_DEALLOCS.load(Ordering::SeqCst), hal_deallocations);

        let report = card.service(22, PcmServiceBudget::uniform(4));
        assert_eq!(report.state, PcmCardState::Resetting);
        assert!(!report.transport_stopped);
        assert_eq!(PCM_DEALLOCS.load(Ordering::SeqCst), deallocations);
        assert_eq!(HAL_DEALLOCS.load(Ordering::SeqCst), hal_deallocations);

        {
            let mut state = state.lock().unwrap();
            state.sticky_reset = false;
            state.status = DeviceStatus::empty();
        }
        let report = card.service(23, PcmServiceBudget::uniform(4));
        assert_eq!(report.state, PcmCardState::Stopped);
        assert!(report.transport_stopped);
        assert_eq!(
            PCM_DEALLOCS.load(Ordering::SeqCst),
            PCM_ALLOCS.load(Ordering::SeqCst)
        );
        assert_eq!(
            HAL_DEALLOCS.load(Ordering::SeqCst),
            HAL_ALLOCS.load(Ordering::SeqCst)
        );
    }

    {
        reset_counts();
        let (mut card, state) = new_card();
        let capabilities = discover(&mut card);
        assert!(capabilities.capture.is_some());
        let stream = card.open(PcmDirection::Capture).unwrap();
        let _ = card.configure(stream, config(), 30, 1_000).unwrap();
        let (result, _) = control_round(&mut card, 31, PcmServiceBudget::new(4, 0, 0, 0));
        result.unwrap();
        let _ = card.prepare(stream, 32, 1_000).unwrap();
        let (result, _) = control_round(&mut card, 33, PcmServiceBudget::new(4, 0, 0, 0));
        result.unwrap();
        let _ = card.start(stream, 34, 1_000).unwrap();
        let (result, _) = control_round(&mut card, 35, PcmServiceBudget::new(4, 0, 0, 0));
        result.unwrap();

        let report = card.service(36, PcmServiceBudget::new(0, 0, 1, 0));
        assert_eq!(report.error, None);
        let mut captures = take_capture_buffers(&mut card);
        assert_eq!(captures.len(), 1);
        let buffer = captures.pop().unwrap();

        {
            let mut state = state.lock().unwrap();
            state.rx_mode = RxMode::Drop;
            state.sticky_reset = true;
        }
        card.recycle_capture(stream, buffer, 40).unwrap();

        let deallocations = PCM_DEALLOCS.load(Ordering::SeqCst);
        let hal_deallocations = HAL_DEALLOCS.load(Ordering::SeqCst);
        let report = card.service(41, PcmServiceBudget::new(0, 0, 0, 0));
        assert_eq!(report.error, Some(PcmError::Timeout));
        assert_eq!(card.state(), PcmCardState::Resetting);
        assert_eq!(PCM_DEALLOCS.load(Ordering::SeqCst), deallocations);
        assert_eq!(HAL_DEALLOCS.load(Ordering::SeqCst), hal_deallocations);

        {
            let mut state = state.lock().unwrap();
            state.sticky_reset = false;
            state.status = DeviceStatus::empty();
        }
        let report = card.service(42, PcmServiceBudget::uniform(8));
        assert_eq!(report.state, PcmCardState::Stopped);
        assert!(report.transport_stopped);
        assert_eq!(
            PCM_DEALLOCS.load(Ordering::SeqCst),
            PCM_ALLOCS.load(Ordering::SeqCst)
        );
        assert_eq!(
            HAL_DEALLOCS.load(Ordering::SeqCst),
            HAL_ALLOCS.load(Ordering::SeqCst)
        );
    }
}

#[test]
fn bad_used_length_quarantines_until_confirmed_reset() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_counts();
    let (mut card, state) = new_card();
    let capabilities = discover(&mut card);
    assert!(capabilities.playback.is_some());
    let stream = card.open(PcmDirection::Playback).unwrap();
    let _ = card.configure(stream, config(), 10, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 11, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let _ = card.prepare(stream, 12, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 13, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();
    let _ = card.start(stream, 14, 1_000).unwrap();
    let (result, _) = control_round(&mut card, 15, PcmServiceBudget::new(4, 0, 0, 0));
    result.unwrap();

    {
        let mut state = state.lock().unwrap();
        state.tx_mode = TxMode::BadLength;
        state.sticky_reset = true;
    }
    let deallocations = PCM_DEALLOCS.load(Ordering::SeqCst);
    let hal_deallocations = HAL_DEALLOCS.load(Ordering::SeqCst);
    let _ = card
        .write_playback(stream, &vec![0x44; PERIOD_BYTES], 1_000)
        .unwrap();
    let report = card.service(16, PcmServiceBudget::uniform(8));
    assert_eq!(report.state, PcmCardState::Resetting);
    assert_eq!(report.error, Some(PcmError::Protocol));
    assert_eq!(PCM_DEALLOCS.load(Ordering::SeqCst), deallocations);
    assert_eq!(HAL_DEALLOCS.load(Ordering::SeqCst), hal_deallocations);
    while let Some(completion) = card.take_completion() {
        assert!(!matches!(completion, PcmCompletion::Playback { .. }));
    }

    {
        let mut state = state.lock().unwrap();
        state.sticky_reset = false;
        state.status = DeviceStatus::empty();
    }
    assert_eq!(HAL_DEALLOCS.load(Ordering::SeqCst), hal_deallocations);
    let report = card.service(17, PcmServiceBudget::uniform(4));
    assert_eq!(report.state, PcmCardState::Stopped);
    assert_eq!(
        PCM_DEALLOCS.load(Ordering::SeqCst),
        PCM_ALLOCS.load(Ordering::SeqCst)
    );
    assert_eq!(
        HAL_DEALLOCS.load(Ordering::SeqCst),
        HAL_ALLOCS.load(Ordering::SeqCst)
    );
}

#[test]
fn release_waits_for_capture_drain_and_cancels_short_rx() {
    let _guard = TEST_LOCK.lock().unwrap();
    reset_counts();
    for data_len in [0, PERIOD_BYTES / 4] {
        let (mut card, state) = new_card();
        let capabilities = discover(&mut card);
        assert!(capabilities.capture.is_some());
        let stream = card.open(PcmDirection::Capture).unwrap();
        let _ = card.configure(stream, config(), 10, 1_000).unwrap();
        let (result, _) = control_round(&mut card, 11, PcmServiceBudget::new(4, 0, 0, 0));
        result.unwrap();

        state.lock().unwrap().rx_mode = RxMode::ShortFrame(data_len);
        let _ = card.prepare(stream, 12, 1_000).unwrap();
        let (result, other) = control_round(&mut card, 13, PcmServiceBudget::new(4, 0, 0, 0));
        result.unwrap();
        assert!(other.is_empty());

        let _ = card.release(stream, 14, 1_000).unwrap();
        let mut cancelled = 0;
        let mut release_completed = false;
        for step in 0..usize::from(PERIOD_COUNT) {
            let budget = if step == 0 {
                PcmServiceBudget::new(4, 0, 1, 0)
            } else {
                PcmServiceBudget::new(0, 0, 1, 0)
            };
            let _ = card.service(15, budget);
            while let Some(completion) = card.take_completion() {
                match completion {
                    PcmCompletion::CaptureCancelled { .. } => cancelled += 1,
                    PcmCompletion::Control { result, .. } => {
                        assert_eq!(step + 1, usize::from(PERIOD_COUNT));
                        result.unwrap();
                        release_completed = true;
                    }
                    PcmCompletion::Fault(error) => panic!("unexpected fault: {error:?}"),
                    _ => panic!("unexpected completion"),
                }
            }
            if step + 1 < usize::from(PERIOD_COUNT) {
                assert!(!release_completed);
                assert_eq!(card.open(PcmDirection::Capture), Err(PcmError::Busy));
            }
        }
        assert_eq!(cancelled, usize::from(PERIOD_COUNT));
        assert!(release_completed);
        assert_eq!(card.state(), PcmCardState::Ready);
        let reopened = card.open(PcmDirection::Capture).unwrap();
        assert_ne!(reopened, stream);

        shutdown(&mut card, 20);
        assert_eq!(
            PCM_DEALLOCS.load(Ordering::SeqCst),
            PCM_ALLOCS.load(Ordering::SeqCst)
        );
        assert_eq!(
            HAL_DEALLOCS.load(Ordering::SeqCst),
            HAL_ALLOCS.load(Ordering::SeqCst)
        );
    }
}
