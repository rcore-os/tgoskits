use alloc::sync::Arc;
use axfs_ng_vfs::{DeviceId, Filesystem, NodeType, VfsResult};
use axsync::{Mutex, RawMutex};
use rand::{RngCore, SeedableRng, rngs::SmallRng};

use super::{
    dynamic::{DirMaker, DynamicDir, DynamicFs},
    file::{Device, DeviceOps},
};

const RANDOM_SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

pub fn new_devfs() -> Filesystem<RawMutex> {
    DynamicFs::new_with("devdevtmpfs".into(), 0x01021994, builder)
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

struct Random {
    rng: Mutex<SmallRng>,
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
        "random",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 8),
            Random {
                rng: Mutex::new(SmallRng::from_seed(*RANDOM_SEED)),
            },
        ),
    );
    root.add(
        "urandom",
        Device::new(
            fs.clone(),
            NodeType::CharacterDevice,
            DeviceId::new(1, 9),
            Random {
                rng: Mutex::new(SmallRng::from_seed(*RANDOM_SEED)),
            },
        ),
    );
    root.build()
}
