//! Special devices

mod axivc;
mod card0;
#[cfg(feature = "rknpu")]
pub(crate) mod card1;
// The real contiguous coherent dma-heap is shared by every accelerator that
// exchanges buffers (JPU / NPU / RGA).
#[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
mod dmaheap;
mod drm;
#[cfg(feature = "input")]
pub mod event;
mod fb;
#[cfg(feature = "sg2002")]
pub mod ion;
#[cfg(any(feature = "input", feature = "k230-kpu"))]
mod irq_service;
mod kmsg;
#[cfg(feature = "k230-kpu")]
mod kpu;
#[cfg(feature = "dev-log")]
mod log;
pub(crate) mod r#loop;
#[cfg(feature = "memtrack")]
mod memtrack;
#[cfg(feature = "jpeg")]
mod mpp_service;
#[cfg(feature = "sg2002")]
mod pinmux;
#[cfg(any(feature = "sg2002", feature = "rk3588-pwm"))]
pub(super) mod pwm;
#[cfg(feature = "rga")]
pub(crate) mod rga;
mod rtc;
#[cfg(feature = "sg2002")]
pub mod tpu;
pub mod tty;

#[cfg(feature = "sg2002-cvi-usb-camera")]
mod cvi_jpu;

#[cfg(feature = "sg2002-cvi-usb-camera")]
mod cvi_usb_camera;

#[cfg(feature = "sg2002-cvi-usb-camera")]
mod cvi_vdec;

use alloc::{format, sync::Arc};
use core::any::Any;

use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsError, VfsResult};

#[cfg(feature = "sg2002")]
pub static ION_DEVICE: OnceLock<Arc<ion::IonDevice>> = OnceLock::new();
#[cfg(feature = "dev-log")]
pub use log::bind_dev_log;

use crate::pseudofs::{Device, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFile, SimpleFs};

static INITIAL_PTS_INSTANCE: OnceLock<Arc<tty::PtsInstance>> = OnceLock::new();

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

pub(crate) fn new_devptsfs(mount: tty::DevPtsMount) -> Filesystem {
    SimpleFs::new_with("devpts".into(), 0x00001cd1, move |fs| {
        devpts_builder(fs, mount)
    })
}

fn devpts_builder(fs: Arc<SimpleFs>, mount: tty::DevPtsMount) -> DirMaker {
    let instance = match mount {
        tty::DevPtsMount::Legacy(options) => initial_pts_instance(options),
        tty::DevPtsMount::NewInstance(options) => tty::PtsInstance::new(options),
    };
    SimpleDir::new_maker(fs.clone(), Arc::new(tty::PtsDir::new(fs, instance)))
}

fn initial_pts_instance(options: tty::DevPtsOptions) -> Arc<tty::PtsInstance> {
    let instance = INITIAL_PTS_INSTANCE
        .call_once(|| tty::PtsInstance::new(options))
        .clone();
    instance.update_options(options);
    instance
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
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM | NodeFlags::BLOCKING
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
        Err(VfsError::Io)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::Io)
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

/// `/dev/random`: reads wait for the CRNG like `getrandom(buf, len, 0)`.
struct Random;

impl DeviceOps for Random {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        // The file layer turns `WouldBlock` into EAGAIN for O_NONBLOCK readers
        // and parks the others on `RandomReady`, as `random_read_iter()` does.
        if !crate::random::rng_is_initialized() {
            return Err(VfsError::WouldBlock);
        }
        crate::random::get_random_bytes(buf);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        crate::random::write_pool(buf);
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_pollable(&self) -> Option<&dyn axpoll::Pollable> {
        Some(&crate::random::RandomReady)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

/// `/dev/urandom`: reads never wait. Linux `urandom_fops` has no poll hook,
/// so it keeps the default always-ready mask.
struct Urandom;

impl DeviceOps for Urandom {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        crate::random::urandom_read(buf);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        crate::random::write_pool(buf);
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
        Err(VfsError::StorageFull)
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
        Err(VfsError::InvalidInput)
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
    let pts_instance = initial_pts_instance(tty::DevPtsOptions::root());

    // Linux environments conventionally expose descriptor paths through
    // these links into procfs (proc_pid_fd(5)). Bash process substitution and
    // the generated NixOS stage-2 initializer rely on the dynamic /dev/fd/N
    // form before systemd can perform any additional /dev setup.
    root.add("fd", descriptor_symlink(fs.clone(), "/proc/self/fd"));
    root.add("stdin", descriptor_symlink(fs.clone(), "/proc/self/fd/0"));
    root.add("stdout", descriptor_symlink(fs.clone(), "/proc/self/fd/1"));
    root.add("stderr", descriptor_symlink(fs.clone(), "/proc/self/fd/2"));
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
            Arc::new(Random),
        ),
    );
    root.add(
        "urandom",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 9),
            Arc::new(Urandom),
        ),
    );
    // Root block device node. Its rdev must equal the root filesystem's st_dev
    // so that tools resolving the root device by scanning /dev (e.g. busybox
    // `rdev`, which stats "/" then looks for a block node with a matching
    // st_rdev) can find it. The root mount is the first mount, so its
    // `DEVICE_COUNTER` id is 1 (== `DeviceId::new(0, 1).0`).
    root.add(
        ax_fs_ng::root::root_block_identity().name,
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
            Arc::new(tty::Ptmx::new(fs.clone(), pts_instance.clone())),
        ),
    );
    root.add(
        "pts",
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(tty::PtsDir::new(fs.clone(), pts_instance)),
        ),
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

    axivc::register_devices(&mut root, fs.clone());

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
    // Mount point for mqueuefs; `mount_all` mounts it at `/dev/mqueue`.
    root.add(
        "mqueue",
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

    #[cfg(feature = "rga")]
    root.add(
        "rga",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(252, 16), // CONFIRM ON BOARD: real /dev/rga major/minor
            Arc::new(rga::RgaDevice::new()),
        ),
    );

    #[cfg(feature = "rknpu")]
    {
        // RockChip-specific NPU companion card (DRM card1). The contiguous
        // `/dev/dma_heap` it allocates from is registered above under the shared
        // accelerator gate.
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
        #[cfg(feature = "sg2002-cvi-usb-camera")]
        {
            let jpu = Arc::new(cvi_jpu::CviJpu::new());
            root.add(
                "cvi-usb-camera0",
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    DeviceId::new(10, 202),
                    Arc::new(cvi_usb_camera::CviCamera::new(jpu.clone())),
                ),
            );
            root.add(
                "cvi_vc_dec0",
                Device::new(
                    fs.clone(),
                    NodeType::CharacterDevice,
                    DeviceId::new(10, 203),
                    Arc::new(cvi_vdec::CviVdec::new(jpu)),
                ),
            );
        }
    }
    SimpleDir::new_maker(fs, Arc::new(root))
}

fn descriptor_symlink(fs: Arc<SimpleFs>, target: &'static str) -> Arc<SimpleFile> {
    SimpleFile::new(fs, NodeType::Symlink, move || Ok(target))
}
