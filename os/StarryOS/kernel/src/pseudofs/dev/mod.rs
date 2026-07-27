//! Special devices

mod card0;
#[cfg(feature = "rknpu")]
mod card1;
// The real contiguous coherent dma-heap is shared by every accelerator that
// exchanges buffers (JPU / NPU / RGA).
#[cfg(any(feature = "jpeg", feature = "rknpu", feature = "rga"))]
mod dmaheap;
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
#[cfg(feature = "rga")]
pub(crate) mod rga;
#[cfg(feature = "ext4")]
pub use r#loop::LoopDevice;
#[cfg(feature = "sg2002")]
pub mod ion;
#[cfg(feature = "memtrack")]
mod memtrack;
#[cfg(feature = "sg2002")]
mod pinmux;
#[cfg(any(feature = "sg2002", feature = "rk3588-pwm"))]
pub(super) mod pwm;
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
use core::{
    any::Any,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_errno::AxError;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsResult};
#[cfg(feature = "sg2002")]
use spin::Once;

#[cfg(feature = "sg2002")]
pub static ION_DEVICE: Once<Arc<ion::IonDevice>> = Once::new();
#[cfg(feature = "dev-log")]
pub use log::bind_dev_log;
use rand::{Rng, SeedableRng, rngs::ChaCha20Rng};

use crate::pseudofs::{Device, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFs};

const RANDOM_SEED_STEP: u64 = 0x9e37_79b9_7f4a_7c15;

static RANDOM_SEED_COUNTER: AtomicU64 = AtomicU64::new(0xa076_1d64_78bd_642f);

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
    state: Mutex<RandomState>,
}

impl Random {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RandomState::new(random_seed())),
        }
    }

    #[cfg(any(test, axtest))]
    fn new_with_seed_for_test(seed: [u8; 32]) -> Self {
        Self {
            state: Mutex::new(RandomState::new(seed)),
        }
    }
}

struct RandomState {
    rng: ChaCha20Rng,
    reseed_count: u64,
}

impl RandomState {
    fn new(seed: [u8; 32]) -> Self {
        Self {
            rng: ChaCha20Rng::from_seed(seed),
            reseed_count: 0,
        }
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        self.rng.fill_bytes(buf);
    }

    fn mix_entropy(&mut self, entropy: &[u8]) {
        let mut seed = [0; 32];
        self.rng.fill_bytes(&mut seed);

        self.reseed_count = self.reseed_count.wrapping_add(1);
        fold_seed_word(&mut seed, entropy.len() as u64);
        fold_seed_word(&mut seed, self.reseed_count);
        fold_seed_word(&mut seed, time_entropy());

        for (idx, byte) in entropy.iter().copied().enumerate() {
            let seed_idx = idx % seed.len();
            seed[seed_idx] ^= byte.rotate_left((idx & 7) as u32);
        }

        self.rng = ChaCha20Rng::from_seed(seed);
    }
}

impl DeviceOps for Random {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        self.state.lock().fill_bytes(buf);
        Ok(buf.len())
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        self.state.lock().mix_entropy(buf);
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}

fn random_seed() -> [u8; 32] {
    // This counter only perturbs seeds created in the same timer tick; it does
    // not publish state to other threads.
    let counter = RANDOM_SEED_COUNTER.fetch_add(RANDOM_SEED_STEP, Ordering::Relaxed);
    let stack_addr = &counter as *const u64 as usize as u64;
    let mut state = time_entropy() ^ counter ^ stack_addr.rotate_left(17);
    let mut seed = [0; 32];

    for chunk in seed.as_chunks_mut::<{ core::mem::size_of::<u64>() }>().0 {
        state = splitmix64(state.wrapping_add(RANDOM_SEED_STEP));
        chunk.copy_from_slice(&state.to_le_bytes());
    }

    seed
}

fn time_entropy() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

fn fold_seed_word(seed: &mut [u8; 32], word: u64) {
    let mixed = splitmix64(word);
    for (idx, byte) in mixed.to_le_bytes().into_iter().enumerate() {
        let seed_idx = idx * 4 % seed.len();
        seed[seed_idx] ^= byte;
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[cfg(axtest)]
pub(crate) fn random_write_mixes_entropy_for_test() -> bool {
    let seed = *b"0123456789abcdef0123456789abcdef";
    let baseline = Random::new_with_seed_for_test(seed);
    let mixed = Random::new_with_seed_for_test(seed);
    let mut discarded = [0; 32];
    let mut baseline_next = [0; 32];
    let mut mixed_next = [0; 32];

    if baseline.read_at(&mut discarded, 0) != Ok(discarded.len()) {
        return false;
    }
    if mixed.read_at(&mut discarded, 0) != Ok(discarded.len()) {
        return false;
    }
    if mixed.write_at(b"caller entropy", 0) != Ok(14) {
        return false;
    }
    if baseline.read_at(&mut baseline_next, 0) != Ok(baseline_next.len()) {
        return false;
    }
    if mixed.read_at(&mut mixed_next, 0) != Ok(mixed_next.len()) {
        return false;
    }

    baseline_next != mixed_next
        && splitmix64_determinism_rules_hold()
        && fold_seed_word_xors_into_byte_indices()
}

#[cfg(axtest)]
fn splitmix64_determinism_rules_hold() -> bool {
    // splitmix64 is a pure bijection: the same input always yields the same
    // 64-bit output (deterministic PRNG), and distinct inputs yield distinct
    // outputs (no fixed-point within a small sample).
    let a = splitmix64(0);
    let b = splitmix64(1);
    let c = splitmix64(0xffff_ffff_ffff_ffff);
    a == splitmix64(0)
        && b == splitmix64(1)
        && c == splitmix64(0xffff_ffff_ffff_ffff)
        && a != b
        && b != c
        && a != c
}

#[cfg(axtest)]
fn fold_seed_word_xors_into_byte_indices() -> bool {
    // fold_seed_word XORs splitmix64(word) into seed[idx*4 % 32]. Repeatedly
    // folding the same word twice must cancel out (XOR is its own inverse).
    let mut seed = [0u8; 32];
    let snapshot_before = seed;
    fold_seed_word(&mut seed, 0x1234_5678_9abc_def0);
    let mutated = seed;
    // Folding again with the same word must restore the original bytes.
    fold_seed_word(&mut seed, 0x1234_5678_9abc_def0);
    let cancelled = seed == snapshot_before;
    // The mutated seed must be different from the all-zero baseline at least at
    // one byte (proves fold_seed_word actually wrote something).
    let mutated_differs_from_zero = mutated.iter().any(|byte| *byte != 0);
    // Folding word 0 affects byte indices {0, 4, 8, 12, 16, 20, 24, 28}.
    let affected_indices = [0, 4, 8, 12, 16, 20, 24, 28];
    let affected_bytes_differ = affected_indices
        .iter()
        .any(|&idx| mutated.get(idx).copied() != snapshot_before.get(idx).copied());
    cancelled && mutated_differs_from_zero && affected_bytes_differ
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

#[cfg(test)]
mod tests {
    use super::{DeviceOps, Random};

    #[test]
    fn random_write_mixes_entropy_into_stream() {
        let seed = *b"0123456789abcdef0123456789abcdef";
        let baseline = Random::new_with_seed_for_test(seed);
        let mixed = Random::new_with_seed_for_test(seed);
        let mut discarded = [0; 32];
        let mut baseline_next = [0; 32];
        let mut mixed_next = [0; 32];

        assert_eq!(
            baseline.read_at(&mut discarded, 0).unwrap(),
            discarded.len()
        );
        assert_eq!(mixed.read_at(&mut discarded, 0).unwrap(), discarded.len());
        assert_eq!(mixed.write_at(b"caller entropy", 0).unwrap(), 14);
        assert_eq!(
            baseline.read_at(&mut baseline_next, 0).unwrap(),
            baseline_next.len()
        );
        assert_eq!(mixed.read_at(&mut mixed_next, 0).unwrap(), mixed_next.len());

        assert_ne!(baseline_next, mixed_next);
    }
}
