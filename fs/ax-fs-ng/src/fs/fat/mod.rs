mod dir;
mod disk;
mod ff;
mod file;
mod fs;
mod metadata;
mod state;
mod util;

use core::cell::UnsafeCell;

use fatfs::SeekFrom;

pub use self::fs::FatFilesystem;
use self::{disk::SeekableDisk, fs::FatFilesystemGuard};

impl fatfs::IoBase for SeekableDisk {
    type Error = ();
}

impl fatfs::Read for SeekableDisk {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        SeekableDisk::read(self, buf).map_err(|_| ())
    }
}

impl fatfs::Write for SeekableDisk {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        SeekableDisk::write(self, buf).map_err(|_| ())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        SeekableDisk::flush(self).map_err(|_| ())
    }
}

impl fatfs::Seek for SeekableDisk {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let size = self.size();
        let new_pos = match pos {
            SeekFrom::Start(pos) => Some(pos),
            SeekFrom::Current(off) => self.position().checked_add_signed(off),
            SeekFrom::End(off) => size.checked_add_signed(off),
        }
        .ok_or(())?;
        self.set_position(new_pos).map_err(|_| ())?;
        Ok(new_pos)
    }
}

/// A reference to an object within a filesystem.
struct FsRef<T> {
    inner: UnsafeCell<T>,
    owner: usize,
}

impl<T> FsRef<T> {
    fn new(owner: &FatFilesystem, inner: T) -> Self {
        Self {
            inner: UnsafeCell::new(inner),
            owner: core::ptr::from_ref(owner) as usize,
        }
    }

    fn borrow<'a>(&'a self, fs: &'a FatFilesystemGuard<'_>) -> &'a T {
        assert_eq!(
            self.owner,
            fs.owner_id(),
            "FAT handle belongs to another filesystem"
        );
        // SAFETY: this cell owns T. The matching filesystem mutex excludes all
        // writers; both the cell and guard remain borrowed for the result.
        unsafe { &*self.inner.get() }
    }

    fn borrow_mut<'a>(&'a self, fs: &'a mut FatFilesystemGuard<'_>) -> &'a mut T {
        assert_eq!(
            self.owner,
            fs.owner_id(),
            "FAT handle belongs to another filesystem"
        );
        // SAFETY: the only matching guard is exclusively borrowed, preventing
        // another read/write through any cell for this filesystem until return.
        // T cannot be destroyed because self is borrowed for the same lifetime.
        unsafe { &mut *self.inner.get() }
    }
}
