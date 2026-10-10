//! Protocol-backed tests for the RDIF display transaction boundary.
//!
//! The control queue runs in the Linux fire-and-forget model (see the `ctrl`
//! module): the fake host below processes the *whole* accumulated batch when
//! the driver's boundary notify kicks it, mirroring one host drain of the
//! delivered avail ring. Device-side rejections of fire-and-forget commands
//! are log-only, so the ambiguity these tests inject is a stalled host: a
//! teardown drain that cannot be confirmed must reset the device before the
//! caller releases backing.

extern crate std;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    mem::size_of,
    num::NonZeroUsize,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicU16, AtomicU64, Ordering},
};
use std::sync::{Mutex, Weak};

use rdif_display::{
    DisplayController, DisplayEvent, DisplayState, Framebuffer, OutputId, ScanoutBuffer,
};
use rdif_gpu::{
    Backing, BufferDescriptor, Completion, ContextHandle, DmaAddr, DmaDomainId, DmaSegment,
    GpuDevice, GpuError, PixelFormat, Resource3d, VirglOps,
};
use virtio_drivers::{
    BufferDirection, Hal, PAGE_SIZE, PhysAddr, Result as VirtIoResult,
    transport::{DeviceStatus, DeviceType, InterruptStatus, Transport},
};
use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{
    VirtIoGpu, VirtIoGpuDevice,
    wire::{Command, RespDisplayInfo},
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
/// Control-queue depth negotiated with the fake device; must match the
/// driver's `CTRL_QUEUE_SIZE`.
const QUEUE_SIZE: usize = 64;

/// Clock that never advances: the bounded waits' deadlines are never reached,
/// so a host that processes on every kick behaves like an unbounded one.
fn frozen_clock() -> u64 {
    0
}

/// Monotonic clock advancing one second per read, for deterministic timeout
/// tests: the 5 s wait deadline is crossed after six reads.
fn ticking_clock() -> u64 {
    static NOW: AtomicU64 = AtomicU64::new(0);
    NOW.fetch_add(1_000_000_000, Ordering::Relaxed)
}

struct TestHal;

// SAFETY: page allocations are exclusive and page aligned. Device addresses
// are identity-mapped pointers in this single-threaded host protocol test.
unsafe impl Hal for TestHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: layout has nonzero size and valid alignment.
        let pointer = unsafe { alloc::alloc::alloc_zeroed(layout) };
        let pointer = NonNull::new(pointer).unwrap();
        (pointer.as_ptr() as PhysAddr, pointer)
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let layout = Layout::from_size_align(pages * PAGE_SIZE, PAGE_SIZE).unwrap();
        // SAFETY: this is the allocation and exact layout from dma_alloc.
        unsafe { alloc::alloc::dealloc(vaddr.as_ptr(), layout) };
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

struct TestBacking {
    bytes: Box<[u8]>,
    segments: [DmaSegment; 1],
}

impl TestBacking {
    fn new() -> Arc<Self> {
        let bytes = vec![0; (WIDTH * HEIGHT * 4) as usize].into_boxed_slice();
        let segment = DmaSegment::new(
            DmaAddr::from(bytes.as_ptr() as u64),
            NonZeroUsize::new(bytes.len()).unwrap(),
        );
        Arc::new(Self {
            bytes,
            segments: [segment],
        })
    }
}

// SAFETY: bytes has a stable heap address and segments covers it exactly in
// the direct DMA domain. The fake host never writes backing and the test uses
// no concurrent CPU alias; coherent cache synchronization is a no-op.
unsafe impl Backing for TestBacking {
    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn domain_id(&self) -> DmaDomainId {
        DmaDomainId::Direct
    }

    fn segments(&self) -> &[DmaSegment] {
        &self.segments
    }

    fn sync_for_device(&self, _range: Range<usize>) -> Result<(), GpuError> {
        Ok(())
    }

    fn sync_for_cpu(&self, _range: Range<usize>) -> Result<(), GpuError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct QueueAddresses {
    descriptors: usize,
    available: usize,
    used: usize,
}

#[derive(Default)]
struct Host {
    queue: QueueAddresses,
    status: DeviceStatus,
    device_features: u64,
    reset_readback: bool,
    commands: Vec<u32>,
    created_formats: Vec<u32>,
    scanout: u32,
    output_width: u32,
    events_read: u32,
    interrupt_status: InterruptStatus,
    /// One-shot: the next `GET_DISPLAY_INFO` reply is rejected with
    /// `ERR_UNSPEC` (a synchronous command, so the rejection is observable).
    fail_display_info: bool,
    /// One-shot: the next synchronous reply omits the fence echo, so the
    /// response cannot be attributed to the request that caused it.
    strip_sync_fence: bool,
    /// While set, the host never processes delivered commands: every drain
    /// times out, which is the async model's ambiguity trigger.
    stall: bool,
}

impl Host {
    fn response(&self, request: &[u8], command: u32, len: usize) -> Vec<u8> {
        let mut reply = vec![0; len];
        set_word(&mut reply, 0, command);
        set_word(&mut reply, 4, word(request, 4));
        reply[8..16].copy_from_slice(&request[8..16]);
        reply
    }

    fn reply(&mut self, request: &[u8]) -> Vec<u8> {
        let command = word(request, 0);
        self.commands.push(command);
        if command == Command::RESOURCE_CREATE_2D.0 {
            self.created_formats.push(word(request, 28));
        }
        if command == Command::GET_DISPLAY_INFO.0 && self.fail_display_info {
            self.fail_display_info = false;
            return self.response(request, 0x1200, 24); // ERR_UNSPEC
        }
        if command == Command::GET_DISPLAY_INFO.0 {
            let mut reply = self.response(
                request,
                Command::OK_DISPLAY_INFO.0,
                size_of::<RespDisplayInfo>(),
            );
            set_word(&mut reply, 24 + 8, self.output_width.max(WIDTH));
            set_word(&mut reply, 24 + 12, HEIGHT);
            set_word(&mut reply, 24 + 16, 1);
            if self.strip_sync_fence {
                self.strip_sync_fence = false;
                set_word(&mut reply, 4, 0);
            }
            return reply;
        }
        if command == Command::SET_SCANOUT.0 {
            self.scanout = word(request, 44);
        }
        self.response(request, 0x1100 /* OK_NODATA */, 24)
    }

    fn process_queue(&mut self) {
        if self.stall {
            return;
        }
        let queue = self.queue;
        assert_ne!(queue.descriptors, 0);
        // The driver's boundary notify delivers a whole batch; drain every
        // available entry, like one host main-loop pass over the avail ring.
        // SAFETY: queue_set supplies three live, aligned VirtQueue allocations.
        // notify is synchronous; the driver cannot revoke or mutate its chains
        // until this method publishes their used elements below.
        unsafe {
            loop {
                let available_index = &*(queue.available.wrapping_add(2) as *const AtomicU16);
                let used_index = &*(queue.used.wrapping_add(2) as *const AtomicU16);
                if available_index.load(Ordering::Acquire) == used_index.load(Ordering::Acquire) {
                    return;
                }
                let slot = (used_index.load(Ordering::Acquire) as usize) % QUEUE_SIZE;
                let head = (queue.available.wrapping_add(4 + 2 * slot) as *const u16).read();
                let mut index = head;
                let mut request = Vec::new();
                let mut response_pointer = None;
                loop {
                    let descriptor = queue.descriptors.wrapping_add(index as usize * 16);
                    let address = (descriptor as *const u64).read();
                    let length = (descriptor.wrapping_add(8) as *const u32).read() as usize;
                    let flags = (descriptor.wrapping_add(12) as *const u16).read();
                    let next = (descriptor.wrapping_add(14) as *const u16).read();
                    assert_eq!(
                        flags & 4,
                        0,
                        "test queue does not negotiate indirect descriptors"
                    );
                    if flags & 2 != 0 {
                        assert!(response_pointer.is_none());
                        response_pointer = Some((address as *mut u8, length));
                    } else {
                        request.extend_from_slice(core::slice::from_raw_parts(
                            address as *const u8,
                            length,
                        ));
                    }
                    if flags & 1 == 0 {
                        break;
                    }
                    index = next;
                }
                let response = self.reply(&request);
                let (pointer, capacity) = response_pointer.expect("response descriptor");
                assert!(response.len() <= capacity);
                core::ptr::copy_nonoverlapping(response.as_ptr(), pointer, response.len());
                let used_entry = queue.used.wrapping_add(4 + 8 * slot);
                (used_entry as *mut u32).write(u32::from(head));
                (used_entry.wrapping_add(4) as *mut u32).write(response.len() as u32);
                used_index.store(used_index.load(Ordering::Relaxed) + 1, Ordering::Release);
            }
        }
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn set_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

struct TestTransport(Arc<Mutex<Host>>);

impl Transport for TestTransport {
    fn device_type(&self) -> DeviceType {
        DeviceType::GPU
    }
    fn read_device_features(&mut self) -> u64 {
        self.0.lock().unwrap().device_features
    }
    fn write_driver_features(&mut self, _features: u64) {}
    fn max_queue_size(&mut self, _queue: u16) -> u32 {
        QUEUE_SIZE as u32
    }
    fn notify(&mut self, queue: u16) {
        assert_eq!(queue, 0);
        self.0.lock().unwrap().process_queue();
    }
    fn get_status(&self) -> DeviceStatus {
        let mut host = self.0.lock().unwrap();
        if host.status.is_empty() {
            host.reset_readback = true;
        }
        host.status
    }
    fn set_status(&mut self, status: DeviceStatus) {
        let mut host = self.0.lock().unwrap();
        host.status = status;
        if status.is_empty() {
            host.reset_readback = false;
        }
    }
    fn set_guest_page_size(&mut self, _size: u32) {}
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
        assert_eq!((queue, size), (0, QUEUE_SIZE as u32));
        self.0.lock().unwrap().queue = QueueAddresses {
            descriptors: descriptors as usize,
            available: driver_area as usize,
            used: device_area as usize,
        };
    }
    fn queue_unset(&mut self, _queue: u16) {
        let mut host = self.0.lock().unwrap();
        assert!(host.reset_readback, "queue unset before reset confirmation");
        host.queue = QueueAddresses::default();
    }
    fn queue_used(&mut self, _queue: u16) -> bool {
        self.0.lock().unwrap().queue.descriptors != 0
    }
    fn ack_interrupt(&mut self) -> InterruptStatus {
        let mut host = self.0.lock().unwrap();
        core::mem::take(&mut host.interrupt_status)
    }
    fn read_config_generation(&self) -> u32 {
        0
    }
    fn read_config_space<T: FromBytes + IntoBytes>(&self, offset: usize) -> VirtIoResult<T> {
        let mut config = [0u8; 12];
        set_word(&mut config, 0, self.0.lock().unwrap().events_read);
        set_word(&mut config, 8, 1); // one scanout
        T::read_from_bytes(&config[offset..offset + size_of::<T>()])
            .map_err(|_| virtio_drivers::Error::ConfigSpaceTooSmall)
    }
    fn write_config_space<T: IntoBytes + Immutable>(
        &mut self,
        offset: usize,
        value: T,
    ) -> VirtIoResult<()> {
        if offset == 4 {
            let bits = word(value.as_bytes(), 0);
            self.0.lock().unwrap().events_read &= !bits;
        }
        Ok(())
    }
}

fn make_device(
    host: &Arc<Mutex<Host>>,
    clock: fn() -> u64,
) -> VirtIoGpuDevice<TestHal, TestTransport> {
    let raw = VirtIoGpu::<TestHal, _>::new(TestTransport(Arc::clone(host)), clock).unwrap();
    VirtIoGpuDevice::new(
        raw,
        DmaDomainId::Direct,
        VirtIoGpuDevice::<TestHal, TestTransport>::virtual_identity(),
        None,
    )
    .unwrap()
}

fn resource_3d(format: u32) -> Resource3d {
    Resource3d {
        target: 2,
        format,
        bind: 0,
        width: WIDTH,
        height: HEIGHT,
        depth: 1,
        array_size: 1,
        last_level: 0,
        samples: 0,
        flags: 0,
    }
}

#[test]
fn normal_drop_confirms_reset_before_releasing_queue() {
    let host = Arc::new(Mutex::new(Host::default()));
    let raw = VirtIoGpu::<TestHal, _>::new(TestTransport(Arc::clone(&host)), frozen_clock).unwrap();
    assert_ne!(host.lock().unwrap().queue.descriptors, 0);
    drop(raw);
    let host = host.lock().unwrap();
    assert!(host.reset_readback);
    assert_eq!(host.queue.descriptors, 0);
}

/// D2 hardening of the synchronous path: a reply whose fence echo does not
/// match the request cannot be attributed to that request, so the device is
/// reset (stopping all DMA) instead of trusting the response.
#[test]
fn sync_response_with_wrong_fence_resets_the_device() {
    let host = Arc::new(Mutex::new(Host {
        strip_sync_fence: true,
        ..Host::default()
    }));
    let raw = VirtIoGpu::<TestHal, _>::new(TestTransport(Arc::clone(&host)), frozen_clock).unwrap();
    // The first output read issues the stripped-fence `GET_DISPLAY_INFO`.
    let result = VirtIoGpuDevice::<TestHal, TestTransport>::new(
        raw,
        DmaDomainId::Direct,
        VirtIoGpuDevice::<TestHal, TestTransport>::virtual_identity(),
        None,
    );
    assert!(
        result.is_err(),
        "an unattributable reply must fail construction"
    );
    let host = host.lock().unwrap();
    assert!(host.status.is_empty());
    assert!(host.reset_readback);
    assert_eq!(host.queue.descriptors, 0);
}

#[test]
fn context_close_releases_attachments_after_drain() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1,
        ..Host::default()
    }));
    let mut device = make_device(&host, frozen_clock);
    let backing = TestBacking::new();
    let weak = Arc::downgrade(&backing);
    let resource = device
        .create_resource_3d(resource_3d(2), Some(backing.clone()))
        .unwrap();
    drop(backing);
    let context = device.create_context("close", 0).unwrap();
    device.attach_resource(context, resource).unwrap();

    device.destroy_context(context).unwrap();
    device.release_buffer(resource).unwrap();
    assert!(weak.upgrade().is_none());
    let (detached, destroyed, unreferenced) = {
        let host = host.lock().unwrap();
        (
            host.commands.contains(&Command::CTX_DETACH_RESOURCE.0),
            host.commands.contains(&Command::CTX_DESTROY.0),
            host.commands.contains(&Command::RESOURCE_UNREF.0),
        )
    };
    assert!(detached);
    assert!(destroyed);
    assert!(unreferenced);
}

/// The async model's ambiguity trigger: a teardown drain the stalled host
/// never confirms must reset the device (stopping all DMA) before the
/// caller releases the attached backing.
#[test]
fn unconfirmed_context_destroy_resets_before_releasing_backing() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1,
        ..Host::default()
    }));
    let mut device = make_device(&host, ticking_clock);
    let backing = TestBacking::new();
    let weak = Arc::downgrade(&backing);
    let resource = device
        .create_resource_3d(resource_3d(2), Some(backing.clone()))
        .unwrap();
    drop(backing);
    let context = device.create_context("close", 0).unwrap();
    device.attach_resource(context, resource).unwrap();

    host.lock().unwrap().stall = true;
    assert_eq!(device.destroy_context(context), Err(GpuError::DeviceLost));
    assert!(weak.upgrade().is_none());
    let host = host.lock().unwrap();
    assert!(host.status.is_empty());
    assert!(host.reset_readback);
}

#[test]
fn resource_creation_passes_the_format_through() {
    let host = Arc::new(Mutex::new(Host::default()));
    let mut device = make_device(&host, frozen_clock);
    let descriptor = |format| BufferDescriptor::Image2d {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH * 4,
        format,
    };

    let argb = device
        .create_buffer(descriptor(PixelFormat::Argb8888), TestBacking::new())
        .unwrap();
    let xrgb = device
        .create_buffer(descriptor(PixelFormat::Xrgb8888), TestBacking::new())
        .unwrap();
    device.release_buffer(argb).unwrap();
    device.release_buffer(xrgb).unwrap();
    // The device-side formats arrive in ring order: B8G8R8A8 (1) for ARGB8888,
    // B8G8R8X8 (2) for XRGB8888.
    assert_eq!(host.lock().unwrap().created_formats, [1, 2]);
}

#[test]
fn display_change_remains_pending_after_output_query_fails() {
    let host = Arc::new(Mutex::new(Host::default()));
    let mut device = make_device(&host, frozen_clock);
    {
        let mut host = host.lock().unwrap();
        host.output_width = WIDTH + 1;
        host.events_read = 1;
        host.interrupt_status = InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT;
        host.fail_display_info = true;
    }

    assert!(device.service_pending().is_err());
    assert_eq!(host.lock().unwrap().events_read, 0);
    assert_eq!(device.poll_event(), None);
    device.service_pending().unwrap();
    {
        let mut host = host.lock().unwrap();
        host.output_width = WIDTH + 2;
        host.events_read = 1;
        host.interrupt_status = InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT;
    }
    device.service_pending().unwrap();
    assert_eq!(
        device.poll_event(),
        Some(DisplayEvent::OutputChanged(OutputId::new(0)))
    );
    assert_eq!(device.poll_event(), None);
    assert_eq!(
        device.output(OutputId::new(0)).unwrap().modes[0].width,
        WIDTH + 2
    );
}

#[test]
fn scanout_rejects_a_3d_resource_with_an_incompatible_format() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1, // VIRTIO_GPU_F_VIRGL
        ..Host::default()
    }));
    let mut device = make_device(&host, frozen_clock);
    let mode = device
        .output(OutputId::new(0))
        .unwrap()
        .preferred_mode
        .unwrap();
    let argb = device.create_resource_3d(resource_3d(1), None).unwrap();
    assert!(device.check(&scanout_state(argb, mode)).is_err());
    let xrgb = device.create_resource_3d(resource_3d(2), None).unwrap();
    device.check(&scanout_state(xrgb, mode)).unwrap();
    let wider = device
        .create_resource_3d(
            Resource3d {
                width: WIDTH + 1,
                ..resource_3d(2)
            },
            None,
        )
        .unwrap();
    assert!(device.check(&scanout_state(wider, mode)).is_err());
    device.release_buffer(argb).unwrap();
    device.release_buffer(xrgb).unwrap();
    device.release_buffer(wider).unwrap();
}

fn scanout_state(handle: rdif_gpu::BufferHandle, mode: rdif_display::Mode) -> DisplayState {
    DisplayState {
        output: OutputId::new(0),
        mode: Some(mode),
        framebuffer: Some(Framebuffer {
            buffer: ScanoutBuffer::Gpu(handle),
            width: WIDTH,
            height: HEIGHT,
            stride: WIDTH * 4,
            offset: 0,
            format: PixelFormat::Xrgb8888,
        }),
        damage: Vec::new(),
    }
}

fn current_handle(device: &impl DisplayController) -> rdif_gpu::BufferHandle {
    let state = device.current_state(OutputId::new(0)).unwrap().unwrap();
    let framebuffer = state.framebuffer.unwrap();
    let ScanoutBuffer::Gpu(handle) = framebuffer.buffer else {
        panic!("GPU scanout expected")
    };
    handle
}

#[test]
fn test_only_and_release_keep_scanout_and_backing() {
    let host = Arc::new(Mutex::new(Host::default()));
    let mut device = make_device(&host, frozen_clock);
    let descriptor = BufferDescriptor::Image2d {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH * 4,
        format: PixelFormat::Xrgb8888,
    };
    let old_backing = TestBacking::new();
    let old_weak: Weak<TestBacking> = Arc::downgrade(&old_backing);
    let old = device
        .create_buffer(descriptor, old_backing.clone())
        .unwrap();
    drop(old_backing);
    let new_backing = TestBacking::new();
    let new_weak: Weak<TestBacking> = Arc::downgrade(&new_backing);
    let new = device
        .create_buffer(descriptor, new_backing.clone())
        .unwrap();
    drop(new_backing);
    let mode = device
        .output(OutputId::new(0))
        .unwrap()
        .preferred_mode
        .unwrap();
    let old_state = scanout_state(old, mode);
    let next_state = scanout_state(new, mode);
    for _ in 0..4 {
        device.commit(&old_state).unwrap();
    }
    assert_eq!(device.poll_event(), None);
    let old_id = host.lock().unwrap().scanout;
    assert_ne!(old_id, 0);
    assert_eq!(current_handle(&device), old);

    let command_count = host.lock().unwrap().commands.len();
    device.check(&next_state).unwrap(); // TEST_ONLY
    assert_eq!(host.lock().unwrap().commands.len(), command_count);

    // A scanout binding keeps its buffer busy.
    assert_eq!(device.release_buffer(old), Err(GpuError::Busy));
    assert!(old_weak.upgrade().is_some());

    device.commit(&next_state).unwrap();
    assert_eq!(current_handle(&device), new);
    assert_ne!(host.lock().unwrap().scanout, old_id);

    device
        .commit(&DisplayState {
            output: OutputId::new(0),
            mode: None,
            framebuffer: None,
            damage: Vec::new(),
        })
        .unwrap();
    device.release_buffer(old).unwrap();
    device.release_buffer(new).unwrap();
    assert!(old_weak.upgrade().is_none());
    assert!(new_weak.upgrade().is_none());
}

/// The buffer-release drain moved to the caller: with the host stalled the
/// release still submits and succeeds instead of timing out and resetting
/// the device — the OS layer waits for the drain outside the device lock.
#[test]
fn stalled_release_submits_without_resetting_the_device() {
    let host = Arc::new(Mutex::new(Host::default()));
    let mut device = make_device(&host, ticking_clock);
    let descriptor = BufferDescriptor::Image2d {
        width: WIDTH,
        height: HEIGHT,
        stride: WIDTH * 4,
        format: PixelFormat::Xrgb8888,
    };
    let backing = TestBacking::new();
    let weak = Arc::downgrade(&backing);
    let buffer = device.create_buffer(descriptor, backing.clone()).unwrap();
    drop(backing);

    host.lock().unwrap().stall = true;
    // The old contract reset the device here when the inline drain timed
    // out; the submission is fire-and-forget with a fence now, so a stalled
    // host no longer wedges or resets on release.
    let completion = device.release_buffer(buffer).unwrap();
    let Completion::Pending(fence) = completion else {
        panic!("a stalled release must report a pending fence, got {completion:?}")
    };

    // The completion proof is the caller's fence wait now: the fence has
    // not fired under the stall, and the query succeeding proves the device
    // was NOT reset — a lost device fails every operation fast with
    // DeviceLost.
    assert!(!device.fence_completed(fence.get()).unwrap());
    assert!(weak.upgrade().is_none());
}

/// Async submit semantics: `submit` returns a pending fence token, and the
/// completion becomes observable through `completion_status` itself — the
/// query delivers the accumulated batch and pumps, with no service path,
/// IRQ worker or polling loop in between.
///
/// Regression (registration-rollback release): the query used to be a pure
/// level read, so the exclusive rollback wait before `MAIN_GPU` is
/// published — where no service path exists — could never observe its own
/// fenced UNREF complete and always burned its whole budget.
#[test]
fn completion_status_delivers_and_pumps_before_reporting() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1,
        ..Host::default()
    }));
    let mut device = make_device(&host, frozen_clock);
    let context = device.create_context("submit", 0).unwrap();
    device.ctrl_notify();

    let completion = device.submit(context, &[0u8; 8]).unwrap();
    let Completion::Pending(fence) = completion else {
        panic!("submit must return a pending fence, got {completion:?}")
    };
    // The first query observes the completion by itself: delivery and the
    // completion pump happen inside it.
    assert_eq!(
        device.completion_status(completion).unwrap(),
        rdif_gpu::CompletionStatus::Complete
    );
    assert!(device.fence_completed(fence.get()).unwrap());

    // A second submit waits on its own fence, which forces delivery.
    let second = device.submit(context, &[0u8; 8]).unwrap();
    device
        .wait_fence(match second {
            Completion::Pending(fence) => fence.get(),
            Completion::Complete => panic!("submit must return a pending fence"),
        })
        .unwrap();
    assert_eq!(
        device.completion_status(second).unwrap(),
        rdif_gpu::CompletionStatus::Complete
    );
}

/// D3 reset×async: after the device was reset, every operation — including
/// the fence query the sync_file refresher drives — fails fast with
/// `DeviceLost` instead of touching the unregistered queue.
#[test]
fn lost_device_fails_every_operation_fast() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1,
        ..Host::default()
    }));
    let mut device = make_device(&host, ticking_clock);
    let context = device.create_context("lost", 0).unwrap();
    device.ctrl_notify();
    let completion = device.submit(context, &[0u8; 8]).unwrap();

    // Lose the device through an unconfirmable teardown drain.
    host.lock().unwrap().stall = true;
    let other = device.create_context("other", 0).unwrap();
    assert_eq!(device.destroy_context(other), Err(GpuError::DeviceLost));

    let Completion::Pending(fence) = completion else {
        panic!("submit must return a pending fence")
    };
    assert_eq!(
        device.create_buffer(
            BufferDescriptor::Image2d {
                width: WIDTH,
                height: HEIGHT,
                stride: WIDTH * 4,
                format: PixelFormat::Xrgb8888,
            },
            TestBacking::new(),
        ),
        Err(GpuError::DeviceLost)
    );
    assert_eq!(device.submit(context, &[0u8; 8]), Err(GpuError::DeviceLost));
    assert_eq!(device.wait_fence(fence.get()), Err(GpuError::DeviceLost));
    assert_eq!(
        device.fence_completed(fence.get()),
        Err(GpuError::DeviceLost)
    );
    assert_eq!(device.service_pending(), Err(GpuError::DeviceLost));
}

/// A context handle from a lost-and-forgotten table is rejected, not reused.
#[test]
fn stale_context_handle_is_rejected() {
    let host = Arc::new(Mutex::new(Host {
        device_features: 1,
        ..Host::default()
    }));
    let mut device = make_device(&host, frozen_clock);
    let context: ContextHandle = device.create_context("stale", 0).unwrap();
    device.destroy_context(context).unwrap();
    assert_eq!(
        device.submit(context, &[0u8; 8]),
        Err(GpuError::InvalidHandle)
    );
}
