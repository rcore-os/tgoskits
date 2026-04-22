//! `lseek()` tests (SEEK_SET/CUR/END/DATA/HOLE).
//!
//! Note: Cargo only runs these tests once a top-level integration test crate
//! includes this module (see `tests/apitest_runner.rs` and `tests/apitest/mod.rs`).

use std::cell::Cell;

use rsext4::{
    api::{DEFAULT_CREATE_MODE, OpenAccessMode, OpenFlags, OpenHow, ResolveFlags},
    bmalloc::AbsoluteBN,
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

fn new_fs() -> (Jbd2Dev<MockBlockDevice>, Ext4FileSystem) {
    let device = MockBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn open_readonly() -> OpenHow {
    OpenHow {
        access: OpenAccessMode::ReadOnly,
        flags: OpenFlags::empty(),
        mode: 0,
        resolve: ResolveFlags::empty(),
    }
}

fn open_rw_create() -> OpenHow {
    OpenHow {
        access: OpenAccessMode::ReadWrite,
        flags: OpenFlags::CREAT,
        mode: DEFAULT_CREATE_MODE,
        resolve: ResolveFlags::empty(),
    }
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
    let err = lseek(&mut jbd2, &mut fs, &mut sparse, size as i64, SeekWhence::Hole).unwrap_err();
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
