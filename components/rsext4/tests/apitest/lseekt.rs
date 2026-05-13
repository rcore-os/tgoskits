//! `lseek()` tests (SEEK_SET/CUR/END/DATA/HOLE).
//!
//! Note: Cargo only runs these tests once a top-level integration test crate
//! includes this module (see `tests/apitest_runner.rs` and `tests/apitest/mod.rs`).

use std::cell::Cell;

use rsext4::{
    api::SeekWhence,
    bmalloc::AbsoluteBN,
    disknode::Ext4Inode,
    endian::write_u32_le,
    error::{Ext4Error, Ext4Result},
    *,
};

/// In-memory block device used by the API integration tests.
struct MockBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: 1024u32 << rsext4::LOG_BLOCK_SIZE,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: DevBN, _count: u32) -> Ext4Result<()> {
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

    fn write(&mut self, buffer: &[u8], block_id: DevBN, _count: u32) -> Ext4Result<()> {
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

    fn dev_block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn new_fs() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn open_readonly() -> bool {
    false
}

fn open_rw_create() -> bool {
    true
}

fn build_nonextent_file_with_direct_blocks(
    dev: &mut Jbd2Dev<MockBlockDevice>,
    fs: &mut Ext4FileSystem,
    path: &str,
    size_bytes: u64,
    direct_blocks: &[(usize, AbsoluteBN)],
) -> Ext4Result<()> {
    mkfile(dev, fs, path, None, None)?;
    let inode_num = open(dev, fs, path, open_readonly())?.inode_num;
    let sectors_per_block = (fs.block_size as u32) / 512;
    let mut i_block = [0u32; 15];

    for &(slot, block) in direct_blocks {
        assert!(slot < 12, "direct slot {slot} must be in [0, 12)");
        i_block[slot] = block.to_u32()?;
    }

    fs.modify_inode(dev, inode_num, |inode| {
        inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_block = i_block;
        inode.i_size_lo = size_bytes as u32;
        inode.i_size_high = (size_bytes >> 32) as u32;
        inode.i_blocks_lo = (direct_blocks.len() as u32).saturating_mul(sectors_per_block);
        inode.l_i_blocks_high = 0;
    })?;

    Ok(())
}

fn build_nonextent_file_with_single_indirect(
    dev: &mut Jbd2Dev<MockBlockDevice>,
    fs: &mut Ext4FileSystem,
    path: &str,
    size_bytes: u64,
    direct_blocks: &[(usize, AbsoluteBN)],
    indirect_root: AbsoluteBN,
    indirect_entries: &[(usize, AbsoluteBN)],
) -> Ext4Result<()> {
    mkfile(dev, fs, path, None, None)?;
    let inode_num = open(dev, fs, path, open_readonly())?.inode_num;
    let sectors_per_block = (fs.block_size as u32) / 512;
    let mut i_block = [0u32; 15];

    for &(slot, block) in direct_blocks {
        assert!(slot < 12, "direct slot {slot} must be in [0, 12)");
        i_block[slot] = block.to_u32()?;
    }
    i_block[12] = indirect_root.to_u32()?;

    dev.read_block(indirect_root)?;
    let buffer = dev.buffer_mut();
    buffer.fill(0);
    for &(slot, block) in indirect_entries {
        let start = slot * core::mem::size_of::<u32>();
        let end = start + core::mem::size_of::<u32>();
        assert!(
            end <= buffer.len(),
            "indirect slot {slot} exceeds block size"
        );
        write_u32_le(block.to_u32()?, &mut buffer[start..end]);
    }
    dev.write_block(indirect_root, true)?;

    fs.modify_inode(dev, inode_num, |inode| {
        inode.i_flags &= !Ext4Inode::EXT4_EXTENTS_FL;
        inode.i_block = i_block;
        inode.i_size_lo = size_bytes as u32;
        inode.i_size_high = (size_bytes >> 32) as u32;
        inode.i_blocks_lo = ((direct_blocks.len() + indirect_entries.len() + 1) as u32)
            .saturating_mul(sectors_per_block);
        inode.l_i_blocks_high = 0;
    })?;

    Ok(())
}

/// SEEK_SET: normal positions work; negative offsets and over-maxbytes are EINVAL.
/// On error the original offset must be preserved.
#[test]
fn test_lseek_set_semantics() {
    let (mut jbd2, mut fs) = new_fs();

    mkfile(&mut jbd2, &mut fs, "/set.txt", Some(b"0123456789"), None).unwrap();
    let mut file = open(&mut jbd2, &mut fs, "/set.txt", open_readonly()).unwrap();

    let off = lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Set).unwrap();
    assert_eq!(off, 0);
    assert_eq!(file.offset, 0);

    let off = lseek(&mut jbd2, &mut fs, &mut file, 3, SeekWhence::Set).unwrap();
    assert_eq!(off, 3);
    assert_eq!(file.offset, 3);

    let err = lseek(&mut jbd2, &mut fs, &mut file, -1, SeekWhence::Set).unwrap_err();
    assert_eq!(err.code, Errno::EINVAL);
    assert_eq!(file.offset, 3);

    let err = lseek(&mut jbd2, &mut fs, &mut file, i64::MAX, SeekWhence::Set).unwrap_err();
    assert_eq!(err.code, Errno::EINVAL);
    assert_eq!(file.offset, 3);

    umount(fs, &mut jbd2).unwrap();
}

/// SEEK_CUR: (0, SEEK_CUR) is a pure query; underflow is EINVAL.
#[test]
fn test_lseek_cur_semantics() {
    let (mut jbd2, mut fs) = new_fs();

    mkfile(&mut jbd2, &mut fs, "/cur.txt", Some(b"0123456789"), None).unwrap();
    let mut file = open(&mut jbd2, &mut fs, "/cur.txt", open_readonly()).unwrap();

    lseek(&mut jbd2, &mut fs, &mut file, 7, SeekWhence::Set).unwrap();

    let off = lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Cur).unwrap();
    assert_eq!(off, 7);
    assert_eq!(file.offset, 7);

    let err = lseek(&mut jbd2, &mut fs, &mut file, -8, SeekWhence::Cur).unwrap_err();
    assert_eq!(err.code, Errno::EINVAL);
    assert_eq!(file.offset, 7);

    umount(fs, &mut jbd2).unwrap();
}

/// SEEK_END: negative offsets are allowed as long as the resolved absolute position is valid.
#[test]
fn test_lseek_end_semantics() {
    let (mut jbd2, mut fs) = new_fs();

    let payload = b"0123456789ABCDEFGHIJ"; // len = 20
    mkfile(&mut jbd2, &mut fs, "/end.txt", Some(payload), None).unwrap();
    let mut file = open(&mut jbd2, &mut fs, "/end.txt", open_readonly()).unwrap();

    let off = lseek(&mut jbd2, &mut fs, &mut file, -1, SeekWhence::End).unwrap();
    assert_eq!(off, 19);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        -(payload.len() as i64),
        SeekWhence::End,
    )
    .unwrap();
    assert_eq!(off, 0);

    let err = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        -(payload.len() as i64) - 1,
        SeekWhence::End,
    )
    .unwrap_err();
    assert_eq!(err.code, Errno::EINVAL);
    assert_eq!(file.offset, 0);

    umount(fs, &mut jbd2).unwrap();
}

/// SEEK_DATA: ENXIO for pos < 0 or pos >= i_size; otherwise locates the next data range.
#[test]
fn test_lseek_data_semantics() {
    let (mut jbd2, mut fs) = new_fs();

    mkdir(&mut jbd2, &mut fs, "/seek").unwrap();
    let mut file = open(&mut jbd2, &mut fs, "/seek/sparse.bin", open_rw_create()).unwrap();
    lseek(&mut jbd2, &mut fs, &mut file, 8192, SeekWhence::Set).unwrap();
    write_at(&mut jbd2, &mut fs, &mut file, b"X").unwrap();

    lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Set).unwrap();

    let err = lseek(&mut jbd2, &mut fs, &mut file, -1, SeekWhence::Data).unwrap_err();
    assert_eq!(err.code, Errno::ENXIO);
    assert_eq!(file.offset, 0);

    let off = lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Data).unwrap();
    assert_eq!(off, 8192);

    let off = lseek(&mut jbd2, &mut fs, &mut file, 8192, SeekWhence::Data).unwrap();
    assert_eq!(off, 8192);

    let size = file.inode.size();
    let err = lseek(&mut jbd2, &mut fs, &mut file, size as i64, SeekWhence::Data).unwrap_err();
    assert_eq!(err.code, Errno::ENXIO);

    umount(fs, &mut jbd2).unwrap();
}

/// SEEK_HOLE: ENXIO for pos < 0 or pos >= i_size; returns the next hole range (EOF is a hole).
#[test]
fn test_lseek_hole_semantics() {
    let (mut jbd2, mut fs) = new_fs();

    mkdir(&mut jbd2, &mut fs, "/seek").unwrap();
    let mut sparse = open(&mut jbd2, &mut fs, "/seek/sparse.bin", open_rw_create()).unwrap();
    lseek(&mut jbd2, &mut fs, &mut sparse, 8192, SeekWhence::Set).unwrap();
    write_at(&mut jbd2, &mut fs, &mut sparse, b"X").unwrap();

    lseek(&mut jbd2, &mut fs, &mut sparse, 0, SeekWhence::Set).unwrap();

    let err = lseek(&mut jbd2, &mut fs, &mut sparse, -1, SeekWhence::Hole).unwrap_err();
    assert_eq!(err.code, Errno::ENXIO);
    assert_eq!(sparse.offset, 0);

    let off = lseek(&mut jbd2, &mut fs, &mut sparse, 0, SeekWhence::Hole).unwrap();
    assert_eq!(off, 0);

    let off = lseek(&mut jbd2, &mut fs, &mut sparse, 8192, SeekWhence::Hole).unwrap();
    assert_eq!(off, 8193);

    let size = sparse.inode.size();
    let err = lseek(
        &mut jbd2,
        &mut fs,
        &mut sparse,
        size as i64,
        SeekWhence::Hole,
    )
    .unwrap_err();
    assert_eq!(err.code, Errno::ENXIO);

    mkfile(&mut jbd2, &mut fs, "/dense.bin", Some(b"HELLO"), None).unwrap();
    let mut dense = open(&mut jbd2, &mut fs, "/dense.bin", open_readonly()).unwrap();
    let off = lseek(&mut jbd2, &mut fs, &mut dense, 0, SeekWhence::Hole).unwrap();
    assert_eq!(off, 5);

    lseek(&mut jbd2, &mut fs, &mut dense, 0, SeekWhence::Set).unwrap();
    let data = read_at(&mut jbd2, &mut fs, &mut dense, 5).unwrap();
    assert_eq!(data, b"HELLO");

    umount(fs, &mut jbd2).unwrap();
}

#[test]
fn test_lseek_nonextent_direct_blocks() {
    let (mut jbd2, mut fs) = new_fs();
    let block_bytes = fs.block_size as u64;

    let direct0 = fs.alloc_block(&mut jbd2).unwrap();
    let direct2 = fs.alloc_block(&mut jbd2).unwrap();
    build_nonextent_file_with_direct_blocks(
        &mut jbd2,
        &mut fs,
        "/direct-nonextent.bin",
        3 * block_bytes,
        &[(0, direct0), (2, direct2)],
    )
    .unwrap();

    let mut file = open(&mut jbd2, &mut fs, "/direct-nonextent.bin", open_readonly()).unwrap();
    assert!(!file.inode.have_extend_header_and_use_extend());

    let off = lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Data).unwrap();
    assert_eq!(off, 0);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        block_bytes as i64,
        SeekWhence::Data,
    )
    .unwrap();
    assert_eq!(off, 2 * block_bytes);

    let off = lseek(&mut jbd2, &mut fs, &mut file, 0, SeekWhence::Hole).unwrap();
    assert_eq!(off, block_bytes);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        (2 * block_bytes) as i64,
        SeekWhence::Hole,
    )
    .unwrap();
    assert_eq!(off, 3 * block_bytes);

    umount(fs, &mut jbd2).unwrap();
}

#[test]
fn test_lseek_nonextent_single_indirect_blocks() {
    let (mut jbd2, mut fs) = new_fs();
    let block_bytes = fs.block_size as u64;

    let direct11 = fs.alloc_block(&mut jbd2).unwrap();
    let indirect_root = fs.alloc_block(&mut jbd2).unwrap();
    let indirect13 = fs.alloc_block(&mut jbd2).unwrap();
    build_nonextent_file_with_single_indirect(
        &mut jbd2,
        &mut fs,
        "/single-indirect-nonextent.bin",
        14 * block_bytes,
        &[(11, direct11)],
        indirect_root,
        &[(1, indirect13)],
    )
    .unwrap();

    let mut file = open(
        &mut jbd2,
        &mut fs,
        "/single-indirect-nonextent.bin",
        open_readonly(),
    )
    .unwrap();
    assert!(!file.inode.have_extend_header_and_use_extend());

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        (11 * block_bytes) as i64,
        SeekWhence::Data,
    )
    .unwrap();
    assert_eq!(off, 11 * block_bytes);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        (12 * block_bytes) as i64,
        SeekWhence::Data,
    )
    .unwrap();
    assert_eq!(off, 13 * block_bytes);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        (11 * block_bytes) as i64,
        SeekWhence::Hole,
    )
    .unwrap();
    assert_eq!(off, 12 * block_bytes);

    let off = lseek(
        &mut jbd2,
        &mut fs,
        &mut file,
        (13 * block_bytes) as i64,
        SeekWhence::Hole,
    )
    .unwrap();
    assert_eq!(off, 14 * block_bytes);

    umount(fs, &mut jbd2).unwrap();
}
