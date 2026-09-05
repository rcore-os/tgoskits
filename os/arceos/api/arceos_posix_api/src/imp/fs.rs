use alloc::sync::Arc;
use core::{
    ffi::{c_char, c_int},
    mem::size_of,
    time::Duration,
};

use ax_fs_ng::fops::OpenOptions;
use ax_io::{PollState, SeekFrom};

use super::fd_ops::{FileLike, get_file_like};
use crate::{PosixError, PosixResult, ctypes, sync::Mutex, utils::char_ptr_to_str};

const UTIME_NOW: i64 = (1 << 30) - 1;
const UTIME_OMIT: i64 = (1 << 30) - 2;

pub struct File {
    inner: Mutex<ax_fs_ng::fops::File>,
}

pub struct Directory {
    inner: Mutex<ax_fs_ng::fops::Directory>,
}

#[repr(C, packed)]
struct LinuxDirent64Head {
    d_ino: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
}

struct DirBuffer<'a> {
    buf: &'a mut [u8],
    offset: usize,
}

impl<'a> DirBuffer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, offset: 0 }
    }

    fn used_len(&self) -> usize {
        self.offset
    }

    fn remaining_space(&self) -> usize {
        self.buf.len().saturating_sub(self.offset)
    }

    fn write_entry(&mut self, d_ino: u64, d_off: i64, d_type: u8, name: &[u8]) -> bool {
        const NAME_OFFSET: usize = size_of::<LinuxDirent64Head>();

        let name_len = name.len().min(255);
        let reclen = (NAME_OFFSET + name_len + 1).next_multiple_of(8);
        if self.remaining_space() < reclen {
            return false;
        }

        unsafe {
            let entry_ptr = self.buf.as_mut_ptr().add(self.offset);
            entry_ptr
                .cast::<LinuxDirent64Head>()
                .write_unaligned(LinuxDirent64Head {
                    d_ino,
                    d_off,
                    d_reclen: reclen as _,
                    d_type,
                });

            let name_ptr = entry_ptr.add(NAME_OFFSET);
            name_ptr.copy_from_nonoverlapping(name.as_ptr(), name_len);
            name_ptr.add(name_len).write(0);
        }

        self.offset += reclen;
        true
    }
}

fn file_type_to_d_type(ty: ax_fs_ng::fops::FileType) -> u8 {
    match ty {
        ax_fs_ng::fops::FileType::Directory => 4,   // DT_DIR
        ax_fs_ng::fops::FileType::RegularFile => 8, // DT_REG
        ax_fs_ng::fops::FileType::Symlink => 10,    // DT_LNK
        _ => 0,                                     // DT_UNKNOWN
    }
}

fn metadata_to_stat(metadata: ax_fs_ng::fops::FileAttr) -> ctypes::stat {
    let st_mode = ((metadata.node_type as u32) << 12) | metadata.mode.bits() as u32;
    ctypes::stat {
        st_dev: metadata.device as _,
        st_ino: metadata.inode as _,
        st_nlink: metadata.nlink as _,
        st_mode,
        st_uid: metadata.uid as _,
        st_gid: metadata.gid as _,
        st_rdev: metadata.rdev.0 as _,
        st_size: metadata.size as _,
        st_blksize: metadata.block_size as _,
        st_blocks: metadata.blocks as _,
        st_atime: duration_to_timespec(metadata.atime),
        st_mtime: duration_to_timespec(metadata.mtime),
        st_ctime: duration_to_timespec(metadata.ctime),
    }
}

fn duration_to_timespec(duration: Duration) -> ctypes::timespec {
    ctypes::timespec {
        tv_sec: duration.as_secs() as _,
        tv_nsec: duration.subsec_nanos() as _,
    }
}

impl File {
    fn new(inner: ax_fs_ng::fops::File) -> Self {
        Self {
            inner: Mutex::new(inner),
        }
    }

    fn add_to_fd_table(self) -> PosixResult<c_int> {
        super::fd_ops::add_file_like(Arc::new(self))
    }

    fn from_fd(fd: c_int) -> PosixResult<Arc<Self>> {
        let f = super::fd_ops::get_file_like(fd)?;
        f.into_any()
            .downcast::<Self>()
            .map_err(|_| PosixError::EINVAL)
    }
}

impl Directory {
    fn new(inner: ax_fs_ng::fops::Directory) -> Self {
        Self {
            inner: Mutex::new(inner),
        }
    }

    fn add_to_fd_table(self) -> PosixResult<c_int> {
        super::fd_ops::add_file_like(Arc::new(self))
    }

    fn from_fd(fd: c_int) -> PosixResult<Arc<Self>> {
        let f = super::fd_ops::get_file_like(fd)?;
        f.into_any()
            .downcast::<Self>()
            .map_err(|_| PosixError::ENOTDIR)
    }
}

impl FileLike for File {
    fn read(&self, buf: &mut [u8]) -> PosixResult<usize> {
        Ok(self.inner.lock().read(buf)?)
    }

    fn write(&self, buf: &[u8]) -> PosixResult<usize> {
        Ok(self.inner.lock().write(buf)?)
    }

    fn stat(&self) -> PosixResult<ctypes::stat> {
        let metadata = self.inner.lock().get_attr()?;
        Ok(metadata_to_stat(metadata))
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> PosixResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: true,
            read_readiness_version: 0,
            write_readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> PosixResult {
        Ok(())
    }
}

impl FileLike for Directory {
    fn read(&self, _buf: &mut [u8]) -> PosixResult<usize> {
        Err(PosixError::EISDIR)
    }

    fn write(&self, _buf: &[u8]) -> PosixResult<usize> {
        Err(PosixError::EISDIR)
    }

    fn stat(&self) -> PosixResult<ctypes::stat> {
        let st_mode = 0o040755;
        Ok(ctypes::stat {
            st_ino: 1,
            st_nlink: 1,
            st_mode,
            st_uid: 1000,
            st_gid: 1000,
            st_size: 0,
            st_blocks: 0,
            st_blksize: 512,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> PosixResult<PollState> {
        Ok(PollState {
            readable: true,
            writable: false,
            read_readiness_version: 0,
            write_readiness_version: 0,
        })
    }

    fn set_nonblocking(&self, _nonblocking: bool) -> PosixResult {
        Ok(())
    }
}

/// Convert open flags to [`OpenOptions`].
fn flags_to_options(flags: c_int, _mode: ctypes::mode_t) -> OpenOptions {
    let flags = flags as u32;
    let mut options = OpenOptions::new();
    match flags & 0b11 {
        ctypes::O_RDONLY => options.read(true),
        ctypes::O_WRONLY => options.write(true),
        _ => {
            options.read(true);
            options.write(true);
        }
    };
    if flags & ctypes::O_APPEND != 0 {
        options.append(true);
    }
    if flags & ctypes::O_TRUNC != 0 {
        options.truncate(true);
    }
    if flags & ctypes::O_CREAT != 0 {
        options.create(true);
    }
    if flags & ctypes::O_EXCL != 0 {
        options.create_new(true);
    }
    options
}

/// Open a file by `filename` and insert it into the file descriptor table.
///
/// Return its index in the file table (`fd`). Return `EMFILE` if it already
/// has the maximum number of files open.
pub fn sys_open(filename: *const c_char, flags: c_int, mode: ctypes::mode_t) -> c_int {
    let filename = char_ptr_to_str(filename);
    debug!("sys_open <= {filename:?} {flags:#o} {mode:#o}");
    syscall_body!(sys_open, {
        let options = flags_to_options(flags, mode);
        let filename = filename?;
        if (flags as u32) & ctypes::O_DIRECTORY != 0 {
            let dir = ax_fs_ng::fops::Directory::open_dir(filename, &options)?;
            Directory::new(dir).add_to_fd_table()
        } else {
            let file = ax_fs_ng::fops::File::open(filename, &options)?;
            File::new(file).add_to_fd_table()
        }
    })
}

/// Read directory entries from `fd` into Linux-style linux_dirent64 buffer.
///
/// Reference: Starry OS implementation
/// Return number of bytes written on success.
pub unsafe fn sys_getdents64(fd: c_int, buf: *mut u8, len: usize) -> ctypes::ssize_t {
    debug!("sys_getdents64 <= {fd} {:#x} {len}", buf as usize);
    syscall_body!(sys_getdents64, {
        if buf.is_null() || len == 0 {
            return Err(PosixError::EINVAL);
        }

        let dir = Directory::from_fd(fd).map_err(|_| PosixError::EBADF)?;
        let mut dir = dir.inner.lock();

        let out = unsafe { core::slice::from_raw_parts_mut(buf, len) };
        let mut dir_buf = DirBuffer::new(out);

        while let Some(entry) = dir.peek_dir_entry() {
            let d_type = file_type_to_d_type(entry.entry_type());
            if !dir_buf.write_entry(
                entry.inode(),
                entry.next_offset() as i64,
                d_type,
                entry.name_as_bytes(),
            ) {
                return Ok(dir_buf.used_len() as ctypes::ssize_t);
            }
            dir.advance_dir_entry();
        }

        Ok(dir_buf.used_len() as ctypes::ssize_t)
    })
}

/// Set the position of the file indicated by `fd`.
///
/// Return its position after seek.
pub fn sys_lseek(fd: c_int, offset: ctypes::off_t, whence: c_int) -> ctypes::off_t {
    debug!("sys_lseek <= {fd} {offset} {whence}");
    syscall_body!(sys_lseek, {
        let pos = match whence {
            0 => {
                if offset < 0 {
                    return Err(PosixError::EINVAL);
                }
                SeekFrom::Start(offset as _)
            }
            1 => SeekFrom::Current(offset as _),
            2 => SeekFrom::End(offset as _),
            _ => return Err(PosixError::EINVAL),
        };
        if let Ok(file) = File::from_fd(fd) {
            return Ok(file.inner.lock().seek(pos)?);
        }
        let dir = Directory::from_fd(fd)?;
        Ok(dir.inner.lock().seek(pos)?)
    })
}

/// Get the file metadata by `path` and write into `buf`.
///
/// Return 0 if success.
pub unsafe fn sys_stat(path: *const c_char, buf: *mut ctypes::stat) -> c_int {
    let path = char_ptr_to_str(path);
    debug!("sys_stat <= {:?} {:#x}", path, buf as usize);
    syscall_body!(sys_stat, {
        if buf.is_null() {
            return Err(PosixError::EFAULT);
        }
        let st = metadata_to_stat(ax_fs_ng::api::metadata(path?)?);
        unsafe { *buf = st };
        Ok(0)
    })
}

/// Get file metadata by `fd` and write into `buf`.
///
/// Return 0 if success.
pub unsafe fn sys_fstat(fd: c_int, buf: *mut ctypes::stat) -> c_int {
    debug!("sys_fstat <= {} {:#x}", fd, buf as usize);
    syscall_body!(sys_fstat, {
        if buf.is_null() {
            return Err(PosixError::EFAULT);
        }

        unsafe { *buf = get_file_like(fd)?.stat()? };
        Ok(0)
    })
}

/// Update the access and modification times of an open file descriptor.
///
/// A null `times` pointer sets both timestamps to the current wall-clock time.
/// Individual timestamps also support the Linux `UTIME_NOW` and `UTIME_OMIT`
/// values in `tv_nsec`.
///
/// # Safety
///
/// When non-null, `times` must point to two readable [`ctypes::timespec`]
/// values for the duration of this call.
pub unsafe fn sys_futimens(fd: c_int, times: *const ctypes::timespec) -> c_int {
    debug!("sys_futimens <= {fd} {:#x}", times as usize);
    syscall_body!(sys_futimens, {
        let file = File::from_fd(fd)?;
        let (atime, mtime) = unsafe { futimens_times(times)? };
        if atime.is_none() && mtime.is_none() {
            return Ok(0);
        }
        file.inner.lock().set_times(atime, mtime)?;
        Ok(0)
    })
}

unsafe fn futimens_times(
    times: *const ctypes::timespec,
) -> PosixResult<(Option<Duration>, Option<Duration>)> {
    let now = ax_hal::time::wall_time();
    if times.is_null() {
        return Ok((Some(now), Some(now)));
    }

    let times = unsafe { core::slice::from_raw_parts(times, 2) };
    Ok((
        file_time_from_timespec(times[0], now)?,
        file_time_from_timespec(times[1], now)?,
    ))
}

fn file_time_from_timespec(
    timespec: ctypes::timespec,
    now: Duration,
) -> PosixResult<Option<Duration>> {
    match timespec.tv_nsec {
        UTIME_NOW => Ok(Some(now)),
        UTIME_OMIT => Ok(None),
        nanoseconds
            if (0..=u32::MAX as i64).contains(&timespec.tv_sec)
                && (0..1_000_000_000).contains(&nanoseconds) =>
        {
            Ok(Some(Duration::new(
                timespec.tv_sec as u64,
                nanoseconds as u32,
            )))
        }
        _ => Err(PosixError::EINVAL),
    }
}

/// Get the metadata of the symbolic link and write into `buf`.
///
/// Return 0 if success.
pub unsafe fn sys_lstat(path: *const c_char, buf: *mut ctypes::stat) -> ctypes::ssize_t {
    let path = char_ptr_to_str(path);
    debug!("sys_lstat <= {:?} {:#x}", path, buf as usize);
    syscall_body!(sys_lstat, {
        if buf.is_null() {
            return Err(PosixError::EFAULT);
        }
        let st = metadata_to_stat(ax_fs_ng::api::symlink_metadata(path?)?);
        unsafe { *buf = st };
        Ok(0)
    })
}

/// Get the path of the current directory.
#[allow(clippy::unnecessary_cast)] // `c_char` is either `i8` or `u8`
pub fn sys_getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    debug!("sys_getcwd <= {:#x} {}", buf as usize, size);
    syscall_body!(sys_getcwd, {
        if buf.is_null() {
            return Ok(core::ptr::null::<c_char>() as _);
        }
        let dst = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, size as _) };
        let cwd = ax_fs_ng::api::current_dir()?;
        let cwd = cwd.as_bytes();
        if cwd.len() < size {
            dst[..cwd.len()].copy_from_slice(cwd);
            dst[cwd.len()] = 0;
            Ok(buf)
        } else {
            Err(PosixError::ERANGE)
        }
    })
}

/// Rename `old` to `new`
/// If new exists, it is first removed.
///
/// Return 0 if the operation succeeds, otherwise return -1.
pub fn sys_rename(old: *const c_char, new: *const c_char) -> c_int {
    syscall_body!(sys_rename, {
        let old_path = char_ptr_to_str(old)?;
        let new_path = char_ptr_to_str(new)?;
        debug!("sys_rename <= old: {old_path:?}, new: {new_path:?}");
        ax_fs_ng::api::rename(old_path, new_path)?;
        Ok(0)
    })
}

#[cfg(test)]
mod tests {
    use ax_fs_ng::fops::{FileAttr, FilePerm, FileType};

    use super::*;

    #[test]
    fn metadata_to_stat_preserves_filesystem_attributes() {
        let metadata = FileAttr {
            device: 3,
            inode: 17,
            nlink: 2,
            mode: FilePerm::from_bits_retain(0o640),
            node_type: FileType::RegularFile,
            uid: 1001,
            gid: 1002,
            size: 4097,
            block_size: 4096,
            blocks: 16,
            rdev: Default::default(),
            atime: Duration::new(10, 11),
            mtime: Duration::new(12, 13),
            ctime: Duration::new(14, 15),
        };

        let stat = metadata_to_stat(metadata);

        assert_eq!(stat.st_dev, 3);
        assert_eq!(stat.st_ino, 17);
        assert_eq!(stat.st_nlink, 2);
        assert_eq!(stat.st_mode, 0o100640);
        assert_eq!(stat.st_uid, 1001);
        assert_eq!(stat.st_gid, 1002);
        assert_eq!(stat.st_size, 4097);
        assert_eq!(stat.st_blksize, 4096);
        assert_eq!(stat.st_blocks, 16);
        assert_eq!(stat.st_atime.tv_sec, 10);
        assert_eq!(stat.st_atime.tv_nsec, 11);
        assert_eq!(stat.st_mtime.tv_sec, 12);
        assert_eq!(stat.st_mtime.tv_nsec, 13);
        assert_eq!(stat.st_ctime.tv_sec, 14);
        assert_eq!(stat.st_ctime.tv_nsec, 15);
    }

    #[test]
    fn file_time_from_timespec_handles_linux_special_values() {
        let now = Duration::new(30, 40);

        assert_eq!(
            file_time_from_timespec(
                ctypes::timespec {
                    tv_sec: 0,
                    tv_nsec: UTIME_NOW as _,
                },
                now,
            ),
            Ok(Some(now))
        );
        assert_eq!(
            file_time_from_timespec(
                ctypes::timespec {
                    tv_sec: 0,
                    tv_nsec: UTIME_OMIT as _,
                },
                now,
            ),
            Ok(None)
        );
    }

    #[test]
    fn file_time_from_timespec_rejects_invalid_values() {
        let now = Duration::ZERO;

        assert_eq!(
            file_time_from_timespec(
                ctypes::timespec {
                    tv_sec: -1,
                    tv_nsec: 0,
                },
                now,
            ),
            Err(PosixError::EINVAL)
        );
        assert_eq!(
            file_time_from_timespec(
                ctypes::timespec {
                    tv_sec: 0,
                    tv_nsec: 1_000_000_000,
                },
                now,
            ),
            Err(PosixError::EINVAL)
        );
        assert_eq!(
            file_time_from_timespec(
                ctypes::timespec {
                    tv_sec: u32::MAX as i64 + 1,
                    tv_nsec: 0,
                },
                now,
            ),
            Err(PosixError::EINVAL)
        );
    }
}
