use axfs_vfs::{VfsNodeAttr, VfsNodeOps, VfsNodePerm, VfsNodeType, VfsResult};
use core::sync::atomic::{AtomicU64, Ordering};

/// A urandom device behaves like `/dev/urandom`.
///
/// It produces random bytes when read.
pub struct UrandomDev {
    seed: AtomicU64,
}

impl UrandomDev {
    /// Create a new instance of the urandom device.
    pub const fn new(seed: u64) -> Self {
        Self {
            seed: AtomicU64::new(seed),
        }
    }

    /// Create a new instance with a default seed.
    fn new_with_default_seed() -> Self {
        Self::new(0xa2ce_a2ce)
    }

    /// LCG pseudo-random number generator
    fn next_u64(&self) -> u64 {
        let new_seed = self
            .seed
            .load(Ordering::SeqCst)
            .wrapping_mul(6364136223846793005)
            + 1;
        self.seed.store(new_seed, Ordering::SeqCst);
        new_seed
    }
}

impl Default for UrandomDev {
    fn default() -> Self {
        Self::new_with_default_seed()
    }
}

impl VfsNodeOps for UrandomDev {
    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new(
            VfsNodePerm::default_file(),
            VfsNodeType::CharDevice,
            0,
            0,
        ))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        for chunk in buf.chunks_mut(8) {
            let random_value = self.next_u64();
            let bytes = random_value.to_ne_bytes();
            for (i, byte) in chunk.iter_mut().enumerate() {
                if i < bytes.len() {
                    *byte = bytes[i];
                }
            }
        }
        Ok(buf.len())
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn truncate(&self, _size: u64) -> VfsResult {
        Ok(())
    }

    axfs_vfs::impl_vfs_non_dir_default! {}
}
