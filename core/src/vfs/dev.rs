use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FsContext;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeType, VfsResult};
use axsync::{Mutex, RawMutex};
use rand::{RngCore, SeedableRng, rngs::SmallRng};

use super::{
    dynamic::{DirMaker, DynamicDir, DynamicFs},
    file::{Device, DeviceOps},
};

/// The device ID for /dev/rtc0
pub const RTC0_DEVICE_ID: DeviceId = DeviceId::new(250, 0);

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

pub fn new_devfs() -> LinuxResult<Filesystem<RawMutex>> {
    let fs = DynamicFs::new_with("devdevtmpfs".into(), 0x01021994, builder);
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
}

struct Rtc;
impl DeviceOps for Rtc {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
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
}

struct Full;
impl DeviceOps for Full {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(LinuxError::ENOSPC)
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(LinuxError::ENOSPC)
    }
}

fn builder(fs: Arc<DynamicFs>) -> DirMaker {
    let mut root = DynamicDir::builder(fs.clone());
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

    root.add("shm", DynamicDir::builder(fs.clone()).build());

    let builder = root.build();
    Arc::new(move |this| builder(this))
}
