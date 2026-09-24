//! Protocol-backed tests for the RDIF display transaction boundary.

extern crate std;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    alloc::Layout,
    mem::size_of,
    num::NonZeroUsize,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicU16, Ordering},
};
use std::sync::{Mutex, Weak};

use rdif_display::{DisplayController, DisplayState, Framebuffer, OutputId, ScanoutBuffer};
use rdif_gpu::{
    Backing, BufferDescriptor, DmaAddr, DmaDomainId, DmaSegment, GpuDevice, GpuError, PixelFormat,
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
const QUEUE_SIZE: usize = 16;

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
    reset_readback: bool,
    commands: Vec<u32>,
    scanout: u32,
    fail_set_scanout: bool,
    fail_flush: bool,
    fail_detach: bool,
}

impl Host {
    fn reply(&mut self, request: &[u8]) -> Vec<u8> {
        let command = word(request, 0);
        self.commands.push(command);
        let failed = if command == Command::SET_SCANOUT.0 && self.fail_set_scanout {
            self.fail_set_scanout = false;
            true
        } else if command == Command::RESOURCE_FLUSH.0 && self.fail_flush {
            self.fail_flush = false;
            true
        } else if command == Command::RESOURCE_DETACH_BACKING.0 && self.fail_detach {
            self.fail_detach = false;
            true
        } else {
            false
        };
        if failed {
            let mut reply = vec![0; 24];
            set_word(&mut reply, 0, 0x1200); // ERR_UNSPEC
            return reply;
        }
        if command == Command::GET_DISPLAY_INFO.0 {
            let mut reply = vec![0; size_of::<RespDisplayInfo>()];
            set_word(&mut reply, 0, Command::OK_DISPLAY_INFO.0);
            set_word(&mut reply, 24 + 8, WIDTH);
            set_word(&mut reply, 24 + 12, HEIGHT);
            set_word(&mut reply, 24 + 16, 1);
            return reply;
        }
        if command == Command::SET_SCANOUT.0 {
            self.scanout = word(request, 44);
        }
        let mut reply = vec![0; 24];
        set_word(&mut reply, 0, Command::OK_NODATA.0);
        reply
    }

    fn process_queue(&mut self) {
        let queue = self.queue;
        assert_ne!(queue.descriptors, 0);
        // SAFETY: queue_set supplies three live, aligned VirtQueue allocations.
        // notify is synchronous; the driver cannot revoke or mutate its chain
        // until this method publishes a used element below.
        unsafe {
            let available_index = &*(queue.available.wrapping_add(2) as *const AtomicU16);
            let used_index = &*(queue.used.wrapping_add(2) as *const AtomicU16);
            let slot = (used_index.load(Ordering::Acquire) as usize) % QUEUE_SIZE;
            assert_ne!(
                available_index.load(Ordering::Acquire),
                used_index.load(Ordering::Acquire)
            );
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
        0
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
        InterruptStatus::empty()
    }
    fn read_config_generation(&self) -> u32 {
        0
    }
    fn read_config_space<T: FromBytes + IntoBytes>(&self, offset: usize) -> VirtIoResult<T> {
        let mut config = [0u8; 12];
        set_word(&mut config, 8, 1); // one scanout
        T::read_from_bytes(&config[offset..offset + size_of::<T>()])
            .map_err(|_| virtio_drivers::Error::ConfigSpaceTooSmall)
    }
    fn write_config_space<T: IntoBytes + Immutable>(
        &mut self,
        _offset: usize,
        _value: T,
    ) -> VirtIoResult<()> {
        Ok(())
    }
}

#[test]
fn normal_drop_confirms_reset_before_releasing_queue() {
    let host = Arc::new(Mutex::new(Host::default()));
    let raw = VirtIoGpu::<TestHal, _>::new(TestTransport(Arc::clone(&host))).unwrap();
    assert_ne!(host.lock().unwrap().queue.descriptors, 0);
    drop(raw);
    let host = host.lock().unwrap();
    assert!(host.reset_readback);
    assert_eq!(host.queue.descriptors, 0);
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
fn test_only_and_failed_commits_keep_scanout_and_backing() {
    let host = Arc::new(Mutex::new(Host::default()));
    let raw = VirtIoGpu::<TestHal, _>::new(TestTransport(Arc::clone(&host))).unwrap();
    let mut device = VirtIoGpuDevice::new(
        raw,
        DmaDomainId::Direct,
        VirtIoGpuDevice::<TestHal, TestTransport>::virtual_identity(),
        None,
    )
    .unwrap();
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
    device.commit(&old_state).unwrap();
    let old_id = host.lock().unwrap().scanout;
    assert_ne!(old_id, 0);
    assert_eq!(current_handle(&device), old);

    let command_count = host.lock().unwrap().commands.len();
    device.check(&next_state).unwrap(); // TEST_ONLY
    assert_eq!(host.lock().unwrap().commands.len(), command_count);

    host.lock().unwrap().fail_set_scanout = true;
    assert!(device.commit(&next_state).is_err());
    assert_eq!(current_handle(&device), old);
    assert_eq!(host.lock().unwrap().scanout, old_id);
    assert!(old_weak.upgrade().is_some());
    assert_eq!(device.release_buffer(old), Err(GpuError::Busy));

    host.lock().unwrap().fail_flush = true;
    assert!(device.commit(&next_state).is_err());
    assert_eq!(current_handle(&device), old);
    assert_eq!(host.lock().unwrap().scanout, old_id);
    assert!(old_weak.upgrade().is_some());
    assert!(new_weak.upgrade().is_some());

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

    let failing_backing = TestBacking::new();
    let failing_weak = Arc::downgrade(&failing_backing);
    let failing = device
        .create_buffer(descriptor, failing_backing.clone())
        .unwrap();
    drop(failing_backing);
    host.lock().unwrap().fail_detach = true;
    assert_eq!(device.release_buffer(failing), Err(GpuError::DeviceLost));
    assert!(host.lock().unwrap().status.is_empty());
    assert_eq!(host.lock().unwrap().queue.descriptors, 0);
    assert!(failing_weak.upgrade().is_none());
}
