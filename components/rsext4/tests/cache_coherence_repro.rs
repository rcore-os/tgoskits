//! Directory-layer and block-cache regression / coverage tests.
//!
//! `direct_write_invalidates_lru_cache` is a genuine red-on-unfixed regression
//! test for the `BlockDev::write_blocks` cache-invalidation fix: a direct write
//! that bypasses the LRU must invalidate any cached copy of the written block,
//! otherwise the next `read_block` returns stale data. It fails on the pre-fix
//! code and passes after, with no timing/eviction dependence.
//!
//! The remaining two tests are coverage / forward-regression guards for the
//! directory layer (which previously had no unit coverage for the
//! create / insert_dir_entry / mkdir duplicate-guard paths). The PR's axfs-ng
//! `link()`/`create()` flush fix lives in a different crate (the axfs-ng VFS
//! glue) that this `rsext4`-level test crate cannot reach, so that specific fix
//! remains validated by the integration self-compile run rather than here.

use std::cell::Cell;

use rsext4::{
    bmalloc::AbsoluteBN,
    dir::get_inode_with_num,
    error::{Ext4Error, Ext4Result},
    *,
};

/// In-memory block device (mirrors the helper used by the other test suites).
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.len() / self.block_size as usize) as u64
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn setup() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

/// Red-on-unfixed regression for `BlockDev::write_blocks` cache invalidation:
/// a direct (non-journaled) write must invalidate any LRU-cached copy of the
/// block, so the next `read_block` observes the freshly-written data rather
/// than a stale cached copy. Pure logic gap — deterministic, no eviction
/// timing or QEMU workload required.
#[test]
fn direct_write_invalidates_lru_cache() {
    let (mut dev, _fs) = setup();
    // An in-range data block (100 MiB / 4 KiB = 25600 blocks); its exact
    // contents are irrelevant — the test only checks read-after-write coherence.
    let bn = AbsoluteBN::new(2000);

    // 1. Prime the LRU with this block's current on-disk contents.
    dev.read_block(bn).expect("prime read");
    let primed = dev.buffer()[..rsext4::BLOCK_SIZE].to_vec();

    // 2. New contents guaranteed to differ from what is cached.
    let mut fresh = primed.clone();
    fresh[0] ^= 0xFF;

    // 3. Direct (non-journaled) write — routes to BlockDev::write_blocks,
    //    bypassing the LRU.
    dev.write_blocks(&fresh, bn, 1, false)
        .expect("direct write");

    // 4. Read again through the cache.
    //    Fixed:   the entry for `bn` was invalidated -> cache miss -> `fresh`.
    //    Unfixed: stale cache hit -> still `primed` -> assertion fails.
    dev.read_block(bn).expect("reread");
    assert_eq!(
        &dev.buffer()[..rsext4::BLOCK_SIZE],
        &fresh[..],
        "read_block after a direct write_blocks must observe freshly written data, not a stale \
         LRU copy"
    );
}

/// Creating many entries in one directory (more than one directory block worth)
/// must leave every entry resolvable. Exercises insert_dir_entry's block growth
/// and the subsequent lookup path.
#[test]
fn many_entries_remain_visible_after_creation() {
    let (mut dev, mut fs) = setup();
    mkdir(&mut dev, &mut fs, "/big").expect("mkdir /big");

    const N: usize = 256;
    for i in 0..N {
        let path = format!("/big/file{i:04}");
        mkfile(&mut dev, &mut fs, &path, Some(b"x"), None)
            .unwrap_or_else(|e| panic!("mkfile {path} failed: {e:?}"));
    }

    for i in 0..N {
        let path = format!("/big/file{i:04}");
        let found = get_inode_with_num(&mut fs, &mut dev, &path)
            .unwrap_or_else(|e| panic!("lookup {path} errored: {e:?}"));
        assert!(
            found.is_some(),
            "entry {path} must be visible after creation"
        );
    }

    umount(fs, &mut dev).expect("umount");
}

/// A second mkdir on an existing path must not corrupt the parent's link count
/// or leak resources. The duplicate-entry guard runs before any inode/block
/// allocation, so the parent's link count must be unchanged afterwards. Whether
/// the duplicate call returns Ok(existing) or an error is intentionally not
/// asserted.
#[test]
fn duplicate_mkdir_is_idempotent_and_preserves_parent_links() {
    let (mut dev, mut fs) = setup();

    mkdir(&mut dev, &mut fs, "/d").expect("first mkdir /d");
    get_inode_with_num(&mut fs, &mut dev, "/d")
        .expect("lookup /d")
        .expect("/d exists after first mkdir");

    let links_before = get_inode_with_num(&mut fs, &mut dev, "/")
        .expect("lookup /")
        .expect("root exists")
        .1
        .i_links_count;

    let _ = mkdir(&mut dev, &mut fs, "/d");

    let links_after = get_inode_with_num(&mut fs, &mut dev, "/")
        .expect("lookup / again")
        .expect("root exists again")
        .1
        .i_links_count;

    assert_eq!(
        links_after, links_before,
        "duplicate mkdir must not bump parent '/' link count"
    );

    assert!(
        get_inode_with_num(&mut fs, &mut dev, "/d")
            .expect("lookup /d again")
            .is_some(),
        "/d must still resolve after duplicate mkdir"
    );

    umount(fs, &mut dev).expect("umount");
}
