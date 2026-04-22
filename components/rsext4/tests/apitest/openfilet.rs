//! `open()` semantic tests.
//!
//! Focus:
//! - Linux-visible error boundaries for `OpenHow`
//! - create/exclusive/truncate observable behavior
//! - symlink and directory open semantics currently implemented in rsext4

use std::cell::Cell;

use rsext4::{
    api::{DEFAULT_CREATE_MODE, OpenAccessMode, OpenFlags, OpenHow, ResolveFlags},
    bmalloc::AbsoluteBN,
    error::{Ext4Error, Ext4Result},
    *,
};

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
    let device = MockBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let fs = mount(&mut jbd2_dev).expect("mount failed");
    (jbd2_dev, fs)
}

fn how(
    access: OpenAccessMode,
    flags: OpenFlags,
    mode: u16,
    resolve: ResolveFlags,
) -> OpenHow {
    OpenHow {
        access,
        flags,
        mode,
        resolve,
    }
}

fn assert_errno<T>(res: Ext4Result<T>, code: Errno) {
    match res {
        Ok(_) => panic!("expected errno {code:?}, got Ok"),
        Err(e) => assert_eq!(e.code, code),
    }
}

#[test]
fn test_open_not_found_and_create_parent_semantics() {
    let (mut dev, mut fs) = new_fs();

    assert_errno(
        open(
        &mut dev,
        &mut fs,
        "/no/such/file",
        how(OpenAccessMode::ReadWrite, OpenFlags::empty(), 0, ResolveFlags::empty()),
        ),
        Errno::ENOENT,
    );

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/missing_parent/new.txt",
            how(
                OpenAccessMode::ReadWrite,
                OpenFlags::CREAT,
                DEFAULT_CREATE_MODE,
                ResolveFlags::empty(),
            ),
        ),
        Errno::ENOENT,
    );

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_create_excl_and_trunc_functionality() {
    let (mut dev, mut fs) = new_fs();

    let _created = open(
        &mut dev,
        &mut fs,
        "/new.txt",
        how(
            OpenAccessMode::ReadWrite,
            OpenFlags::CREAT,
            DEFAULT_CREATE_MODE,
            ResolveFlags::empty(),
        ),
    )
    .unwrap();

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/new.txt",
            how(
                OpenAccessMode::ReadWrite,
                OpenFlags::CREAT | OpenFlags::EXCL,
                DEFAULT_CREATE_MODE,
                ResolveFlags::empty(),
            ),
        ),
        Errno::EEXIST,
    );

    mkfile(&mut dev, &mut fs, "/trunc.txt", Some(b"abcdef"), None).unwrap();
    let _ = open(
        &mut dev,
        &mut fs,
        "/trunc.txt",
        how(
            OpenAccessMode::ReadWrite,
            OpenFlags::TRUNC,
            0,
            ResolveFlags::empty(),
        ),
    )
    .unwrap();
    let bytes = read_file(&mut dev, &mut fs, "/trunc.txt").unwrap();
    assert!(bytes.is_empty());

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_linux_flag_validation_boundaries() {
    let (mut dev, mut fs) = new_fs();

    assert_errno(
        open(
        &mut dev,
        &mut fs,
        "/x",
        how(OpenAccessMode::ReadOnly, OpenFlags::empty(), 0o644, ResolveFlags::empty()),
        ),
        Errno::EINVAL,
    );

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/x",
            how(
                OpenAccessMode::ReadWrite,
                OpenFlags::CREAT,
                0o17777,
                ResolveFlags::empty(),
            ),
        ),
        Errno::EINVAL,
    );

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/x",
            how(
                OpenAccessMode::ReadWrite,
                OpenFlags::DIRECTORY | OpenFlags::CREAT,
                DEFAULT_CREATE_MODE,
                ResolveFlags::empty(),
            ),
        ),
        Errno::EINVAL,
    );

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/x",
            how(
                OpenAccessMode::ReadOnly,
                OpenFlags::PATH | OpenFlags::TRUNC,
                0,
                ResolveFlags::empty(),
            ),
        ),
        Errno::EINVAL,
    );

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/x",
            how(
                OpenAccessMode::ReadWrite,
                OpenFlags::CREAT,
                DEFAULT_CREATE_MODE,
                ResolveFlags::CACHED,
            ),
        ),
        Errno::EAGAIN,
    );

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_directory_and_root_semantics() {
    let (mut dev, mut fs) = new_fs();

    mkdir(&mut dev, &mut fs, "/d").unwrap();
    mkfile(&mut dev, &mut fs, "/f", Some(b"x"), None).unwrap();

    let _root = open(
        &mut dev,
        &mut fs,
        "/",
        how(OpenAccessMode::ReadOnly, OpenFlags::empty(), 0, ResolveFlags::empty()),
    )
    .unwrap();

    assert_errno(
        open(
        &mut dev,
        &mut fs,
        "/d",
        how(OpenAccessMode::ReadWrite, OpenFlags::empty(), 0, ResolveFlags::empty()),
        ),
        Errno::EISDIR,
    );

    assert_errno(
        open(
        &mut dev,
        &mut fs,
        "/f",
        how(OpenAccessMode::ReadOnly, OpenFlags::DIRECTORY, 0, ResolveFlags::empty()),
        ),
        Errno::ENOTDIR,
    );

    umount(fs, &mut dev).unwrap();
}

#[test]
fn test_open_symlink_semantics_current_contract() {
    let (mut dev, mut fs) = new_fs();

    mkfile(&mut dev, &mut fs, "/target", Some(b"payload"), None).unwrap();
    create_symbol_link(&mut dev, &mut fs, "/target", "/link").unwrap();

    assert_errno(
        open(
            &mut dev,
            &mut fs,
            "/link",
            how(
                OpenAccessMode::ReadOnly,
                OpenFlags::NOFOLLOW,
                0,
                ResolveFlags::empty(),
            ),
        ),
        Errno::ELOOP,
    );

    assert_errno(
        open(
        &mut dev,
        &mut fs,
        "/link",
        how(OpenAccessMode::ReadOnly, OpenFlags::empty(), 0, ResolveFlags::empty()),
        ),
        Errno::EOPNOTSUPP,
    );

    umount(fs, &mut dev).unwrap();
}
