//! Special devices

mod card0;
#[cfg(feature = "rknpu")]
mod card1;
// The real contiguous coherent dma-heap is shared by every accelerator that
// exchanges buffers (JPU / NPU; RGA when its node lands).
#[cfg(any(feature = "jpeg", feature = "rknpu"))]
mod dmaheap;
#[cfg(feature = "dma-heap")]
mod dma_heap;
mod drm;
#[cfg(feature = "input")]
pub mod event;
mod fb;
mod kmsg;
#[cfg(feature = "k230-kpu")]
mod kpu;
#[cfg(feature = "dev-log")]
mod log;
mod r#loop;
#[cfg(feature = "ext4")]
mod loop_block;
#[cfg(feature = "jpeg")]
mod mpp_service;
#[cfg(feature = "ext4")]
pub use r#loop::LoopDevice;
#[cfg(feature = "sg2002")]
pub mod ion;
#[cfg(feature = "memtrack")]
mod memtrack;
#[cfg(feature = "sg2002")]
mod pinmux;
#[cfg(feature = "sg2002")]
pub(super) mod pwm;
mod rtc;
#[cfg(feature = "sg2002")]
pub mod tpu;
pub mod tty;

#[cfg(feature = "sg2002")]
mod cvi_usb_camera;

use alloc::{format, sync::Arc};
use core::any::Any;

use ax_errno::AxError;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsResult};
#[cfg(feature = "sg2002")]
use spin::Once;

#[cfg(feature = "sg2002")]
pub static ION_DEVICE: Once<Arc<ion::IonDevice>> = Once::new();
#[cfg(feature = "dev-log")]
pub use log::bind_dev_log;
use rand::{Rng, SeedableRng, rngs::SmallRng};

use crate::pseudofs::{Device, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFs};

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

#[cfg(any(feature = "sg2002", feature = "k230-kpu"))]
pub(super) struct IrqRegistration {
    handle: ax_runtime::hal::irq::IrqHandle,
}

#[cfg(any(feature = "sg2002", feature = "k230-kpu"))]
impl IrqRegistration {
    pub(super) const fn new(handle: ax_runtime::hal::irq::IrqHandle) -> Self {
        Self { handle }
    }

    pub(super) fn enable(&self) -> Result<(), ax_runtime::hal::irq::IrqError> {
        ax_runtime::hal::irq::enable_irq(self.handle)
    }
}

#[cfg(any(feature = "sg2002", feature = "k230-kpu"))]
impl Drop for IrqRegistration {
    fn drop(&mut self) {
        let _ = ax_runtime::hal::irq::disable_irq(self.handle);
        let _ = ax_runtime::hal::irq::free_irq(self.handle);
    }
}

#[cfg(any(feature = "sg2002", feature = "k230-kpu"))]
pub(super) fn request_shared_disabled(
    irq: ax_runtime::hal::irq::IrqId,
    handler: impl FnMut(ax_runtime::hal::irq::IrqContext) -> ax_runtime::hal::irq::IrqReturn
    + Send
    + 'static,
) -> Result<IrqRegistration, ax_runtime::hal::irq::IrqError> {
    let request = ax_runtime::hal::irq::IrqRequest::new(handler)
        .share_mode(ax_runtime::hal::irq::ShareMode::Shared)
        .auto_enable(ax_runtime::hal::irq::AutoEnable::No);
    ax_runtime::hal::irq::request_irq(irq, request).map(IrqRegistration::new)
}

pub(crate) fn new_devfs() -> Filesystem {
    SimpleFs::new_with("devfs".into(), 0x01021994, builder)
}

struct Null;

impl DeviceOps for Null {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

/// Placeholder root block device. starry has no real block-device backend for
/// the root mount; this node exists only so tools that resolve the root device
/// by scanning /dev (e.g. busybox `rdev`) can find a block node whose `rdev`
/// matches the root filesystem's `st_dev`. Real block I/O is unsupported:
/// read/write return `EIO` rather than silently succeeding, so the node never
/// masquerades as a working disk for `dd`/`blkid`/`fsck`.
struct RootBlk;

impl DeviceOps for RootBlk {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::Io)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::Io)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

struct Zero;

impl DeviceOps for Zero {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Random {
    rng: Mutex<SmallRng>,
}

impl Random {
    pub fn new() -> Self {
        Self {
            rng: Mutex::new(SmallRng::from_seed(*RANDOM_SEED)),
        }
    }
}

impl DeviceOps for Random {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        self.rng.lock().fill_bytes(buf);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct Full;

impl DeviceOps for Full {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::StorageFull)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

struct CpuDmaLatency;

impl DeviceOps for CpuDmaLatency {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

fn builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    root.add(
        "null",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 3),
            Arc::new(Null),
        ),
    );
    root.add(
        "zero",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 5),
            Arc::new(Zero),
        ),
    );
    root.add(
        "full",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 7),
            Arc::new(Full),
        ),
    );
    root.add(
        "random",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 8),
            Arc::new(Random::new()),
        ),
    );
    root.add(
        "urandom",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 9),
            Arc::new(Random::new()),
        ),
    );
    // Root block device node. Its rdev must equal the root filesystem's st_dev
    // so that tools resolving the root device by scanning /dev (e.g. busybox
    // `rdev`, which stats "/" then looks for a block node with a matching
    // st_rdev) can find it. The root mount is the first mount, so its
    // `DEVICE_COUNTER` id is 1 (== `DeviceId::new(0, 1).0`).
    root.add(
        "vda",
        Device::new(
            fs.clone(),
            NodeType::BlockDevice,
            DeviceId::new(0, 1),
            Arc::new(RootBlk),
        ),
    );
    if ax_display::has_display() {
        root.add(
            "fb0",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(29, 0),
                Arc::new(fb::FrameBuffer::new()),
            ),
        );
    }

    root.add(
        "tty",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 0),
            Arc::new(tty::CurrentTty),
        ),
    );
    for entry in tty::serial_tty_entries() {
        let number = entry.number();
        let minor = u32::try_from(64 + number).unwrap_or(u32::MAX);
        root.add(
            format!("ttyS{number}"),
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(4, minor),
                entry.tty(),
            ),
        );
    }
    root.add(
        "console",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 1),
            tty::console_device(),
        ),
    );
    root.add_dynamic("ttyUSB0", {
        let fs = fs.clone();
        move || {
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(188, 0),
                tty::usb_serial_tty(0).expect("ttyUSB0 slot must exist"),
            )
            .into()
        }
    });

    root.add(
        "ptmx",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(5, 2),
            Arc::new(tty::Ptmx(fs.clone())),
        ),
    );
    root.add(
        "pts",
        SimpleDir::new_maker(fs.clone(), Arc::new(tty::PtsDir)),
    );
    #[cfg(feature = "dev-log")]
    root.add(
        "log",
        crate::pseudofs::SimpleFile::new(fs.clone(), NodeType::Socket, || Ok("")),
    );

    #[cfg(feature = "memtrack")]
    root.add(
        "memtrack",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(114, 514),
            Arc::new(memtrack::MemTrack),
        ),
    );

    root.add(
        "cpu_dma_latency",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(10, 1024),
            Arc::new(CpuDmaLatency),
        ),
    );
    // /dev/kmsg — standard char major 1, minor 11 (LANANA memory-device major,
    // same group as null/zero/random above).
    root.add(
        "kmsg",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 11),
            Arc::new(kmsg::Kmsg),
        ),
    );
    root.add(
        "rtc0",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            rtc::RTC0_DEVICE_ID,
            Arc::new(rtc::Rtc),
        ),
    );

    #[cfg(feature = "k230-kpu")]
    {
        if let Some(kpu_device) = kpu::KpuDevice::probe().map(Arc::new) {
            root.add(
                "kpu",
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    kpu::KPU_DEVICE_ID,
                    kpu_device.clone(),
                ),
            );
            root.add(
                "kpu0",
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    kpu::KPU_DEVICE_ID,
                    kpu_device,
                ),
            );
        }
    }

    // /dev/mpp_service — Rockchip MPP-compatible JPEG decoder node. Registered
    // unconditionally under `jpeg`; the node itself reports an error if the
    // hardware was not probed.
    #[cfg(feature = "jpeg")]
    {
        root.add(
            "mpp_service",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                mpp_service::MPP_SERVICE_DEVICE_ID,
                Arc::new(mpp_service::MppService::new()),
            ),
        );
    }

    // /dev/dma_heap — the real contiguous, DMA-coherent allocator that the
    // accelerators share buffers from (zero-copy across JPU / NPU / RGA). Every
    // heap name maps to the same allocator. Available under any accelerator
    // feature, not just `jpeg`.
    #[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
    {
        let mut dma_heap_dir = DirMapping::new();
        for name in dmaheap::HEAP_NAMES {
            dma_heap_dir.add(
                *name,
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    dmaheap::DMA_HEAP_DEVICE_ID,
                    Arc::new(dmaheap::DmaHeap),
                ),
            );
        }
        root.add(
            "dma_heap",
            SimpleDir::new_maker(fs.clone(), Arc::new(dma_heap_dir)),
        );
    }

    // This is mounted to a tmpfs in `new_procfs`
    root.add(
        "shm",
        SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
    );
    {
        let mut bus_dir = DirMapping::new();
        bus_dir.add(
            "usb",
            SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
        );
        root.add("bus", SimpleDir::new_maker(fs.clone(), Arc::new(bus_dir)));
    }

    // /dev/dri/card0 — simpledrm-class DRM character device. Advertised
    // unconditionally so libdrm/libudev see the DRM node even before
    // there's a display device behind it.
    let dri_card0 = card0::Card0::new();
    let mut dri_dir = DirMapping::new();
    dri_dir.add(
        "card0",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(226, 0),
            dri_card0.clone(),
        ),
    );
    dri_dir.add(
        "renderD128",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(226, 128),
            dri_card0,
        ),
    );

    #[cfg(feature = "rknpu")]
    {
        // RockChip-specific NPU companion card (DRM card1).
        dri_dir.add(
            "card1",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                card1::CARD1_SYSTEM_DEVICE_ID,
                Arc::new(card1::Card1::new()),
            ),
        );
    }
    root.add("dri", SimpleDir::new_maker(fs.clone(), Arc::new(dri_dir)));

    // Loop devices (major 7, minor = device index)
    for i in 0..16 {
        let dev_id = DeviceId::new(7, i);
        root.add(
            format!("loop{i}"),
            Device::new(
                fs.clone(),
                NodeType::BlockDevice,
                dev_id,
                Arc::new(r#loop::LoopDevice::new(i, dev_id)),
            ),
        );
    }

    // Input devices
    #[cfg(feature = "input")]
    root.add(
        "input",
        SimpleDir::new_maker(fs.clone(), Arc::new(event::input_devices(fs.clone()))),
    );

    #[cfg(feature = "sg2002")]
    {
        if let Some(tpu) = tpu::TpuDevice::probe() {
            root.add(
                "cvi-tpu0",
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    DeviceId::new(240, 0),
                    Arc::new(tpu),
                ),
            );
        }
        let ion_device = Arc::new(ion::IonDevice::new());
        ION_DEVICE.call_once(|| ion_device.clone());
        root.add(
            "ion",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(10, 56),
                ion_device,
            ),
        );
        root.add(
            "pinmux",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(1, 1),
                Arc::new(pinmux::PinmuxDev),
            ),
        );
        root.add(
            "cvi-usb-camera0",
            Device::new(
                fs.clone(),
                NodeType::CharacterDevice,
                DeviceId::new(10, 202),
                Arc::new(cvi_usb_camera::CviCamera::new()),
            ),
        );
    }
    SimpleDir::new_maker(fs, Arc::new(root))
}
