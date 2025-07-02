use core::{
    alloc::Layout,
    any::Any,
    cmp,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use alloc::{collections::btree_map::BTreeMap, format, sync::Arc, vec::Vec};
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::{File, FsContext};
use axfs_ng_vfs::{DeviceId, Filesystem, NodeType, VfsResult};
use axsync::{Mutex, RawMutex};
use linux_raw_sys::loop_device::loop_info;
use rand::{RngCore, SeedableRng, rngs::SmallRng};

use crate::vfs::simple::{Device, DeviceOps, DirMaker, DirMapping, SimpleDir, SimpleFs};

/// The device ID for /dev/rtc0
const RTC0_DEVICE_ID: DeviceId = DeviceId::new(250, 0);

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

pub(crate) fn new_devfs() -> LinuxResult<Filesystem<RawMutex>> {
    let fs = SimpleFs::new_with("devdevtmpfs".into(), 0x01021994, builder);
    let mp = axfs_ng_vfs::Mountpoint::new_root(&fs);
    FsContext::new(mp.root_location())
        .resolve("/shm")?
        .mount(&super::tmp::MemoryFs::new())?;
    Ok(fs)
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
}

struct Zero;
impl DeviceOps for Zero {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// RTC device
pub struct Rtc;
impl DeviceOps for Rtc {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }
    fn as_any(&self) -> &dyn Any {
        self
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
}

struct Full;
impl DeviceOps for Full {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        buf.fill(0);
        Ok(buf.len())
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(LinuxError::ENOSPC)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

static STAMPED_GENERATION: AtomicU64 = AtomicU64::new(0);

struct MemTrack;
impl DeviceOps for MemTrack {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset == 0 {
            match buf {
                b"start\n" => {
                    let generation = axalloc::current_generation();
                    STAMPED_GENERATION.store(generation, Ordering::SeqCst);
                    info!("Memory allocation generation stamped: {}", generation);
                }
                b"end\n" => {
                    // Wait for gc
                    axtask::yield_now();

                    let from = STAMPED_GENERATION.load(Ordering::SeqCst);
                    let to = axalloc::current_generation();

                    let mut allocations: BTreeMap<_, Vec<Layout>> = BTreeMap::new();
                    axalloc::allocations_in(from..to, |info| {
                        allocations
                            .entry(info.backtrace.clone())
                            .or_default()
                            .push(info.layout);
                    });
                    let mut allocations = allocations
                        .into_iter()
                        .map(|(bt, layouts)| {
                            let total_size = layouts.iter().map(|l| l.size()).sum::<usize>();
                            (bt, layouts, total_size)
                        })
                        .collect::<Vec<_>>();
                    allocations.sort_by_key(|it| cmp::Reverse(it.2));
                    if !allocations.is_empty() {
                        warn!("===========================");
                        warn!("Memory leak detected:");
                        for (backtrace, layouts, total_size) in allocations {
                            warn!(
                                " {}\t bytes, {} allocations, {:?}, {}",
                                total_size,
                                layouts.len(),
                                layouts[0],
                                backtrace
                            );
                        }
                        warn!("==========================");
                    }
                }
                _ => {
                    warn!("Unknown command: {:?}", buf);
                }
            }
        }
        Ok(buf.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// /dev/loopX devices
pub struct LoopDevice {
    number: u32,
    dev_id: DeviceId,
    /// Underlying file for the loop device, if any.
    pub file: Mutex<Option<Arc<Mutex<File<RawMutex>>>>>,
    /// Read-only flag for the loop device.
    pub ro: AtomicBool,
    /// Read-ahead size for the loop device, in bytes.
    pub ra: AtomicU32,
}
impl LoopDevice {
    fn new(number: u32, dev_id: DeviceId) -> Self {
        Self {
            number,
            dev_id,
            file: Mutex::new(None),
            ro: AtomicBool::new(false),
            ra: AtomicU32::new(512),
        }
    }

    /// Get information about the loop device.
    pub fn get_info(&self, dest: &mut loop_info) -> LinuxResult<()> {
        if self.file.lock().is_none() {
            return Err(LinuxError::ENXIO);
        }
        dest.lo_number = self.number as _;
        dest.lo_rdevice = self.dev_id.0 as _;
        Ok(())
    }

    /// Set information for the loop device.
    pub fn set_info(&self, _src: &loop_info) -> LinuxResult<()> {
        Ok(())
    }

    /// Clone the underlying file of the loop device.
    pub fn clone_file(&self) -> VfsResult<Arc<Mutex<File<RawMutex>>>> {
        let file = self.file.lock().clone();
        file.ok_or(LinuxError::ENXIO)
    }
}
impl DeviceOps for LoopDevice {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let file = self.file.lock().clone();
        file.ok_or(LinuxError::EPERM)?.lock().read_at(buf, offset)
    }
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if self.ro.load(Ordering::Relaxed) {
            return Err(LinuxError::EROFS);
        }
        let file = self.file.lock().clone();
        file.ok_or(LinuxError::EPERM)?.lock().write_at(buf, offset)
    }
    fn as_any(&self) -> &dyn Any {
        self
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
            Null,
        ),
    );
    root.add(
        "zero",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 5),
            Zero,
        ),
    );
    root.add(
        "full",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 7),
            Full,
        ),
    );
    root.add(
        "random",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 8),
            Random::new(),
        ),
    );
    root.add(
        "urandom",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 9),
            Random::new(),
        ),
    );
    root.add(
        "rtc0",
        Device::new(fs.clone(), NodeType::CharacterDevice, RTC0_DEVICE_ID, Rtc),
    );
    root.add(
        "memtrack",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(114, 514),
            MemTrack,
        ),
    );

    // This is mounted to a tmpfs in `new_procfs`
    root.add(
        "shm",
        SimpleDir::new_maker(fs.clone(), Arc::new(DirMapping::new())),
    );

    for i in 0..16 {
        let dev_id = DeviceId::new(7, 0);
        root.add(
            format!("loop{i}"),
            Device::new(
                fs.clone(),
                NodeType::BlockDevice,
                dev_id,
                LoopDevice::new(i, dev_id),
            ),
        );
    }

    SimpleDir::new_maker(fs, Arc::new(root))
}
