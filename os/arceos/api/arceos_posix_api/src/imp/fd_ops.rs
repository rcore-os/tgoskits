use alloc::sync::Arc;
use core::ffi::c_int;

use ax_io::PollState;
use flatten_objects::FlattenObjects;
use scope_local::scope_local;

use crate::{
    PosixError, PosixResult, ctypes,
    imp::stdio::{stdin, stdout},
    sync::Mutex,
};

pub const AX_FILE_LIMIT: usize = 1024;

pub(crate) struct FileDescriptor {
    file: Arc<dyn FileLike>,
    close_on_exec: bool,
}

impl FileDescriptor {
    fn new(file: Arc<dyn FileLike>, close_on_exec: bool) -> Self {
        Self {
            file,
            close_on_exec,
        }
    }
}

#[allow(dead_code)]
pub trait FileLike: Send + Sync {
    fn read(&self, buf: &mut [u8]) -> PosixResult<usize>;
    fn write(&self, buf: &[u8]) -> PosixResult<usize>;
    fn stat(&self) -> PosixResult<ctypes::stat>;
    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync>;
    fn poll(&self) -> PosixResult<PollState>;
    fn set_nonblocking(&self, nonblocking: bool) -> PosixResult;
}

scope_local! {
    pub(crate) static FD_TABLE: Arc<Mutex<FlattenObjects<FileDescriptor, AX_FILE_LIMIT>>> = Arc::new(Mutex::new({
        let mut fd_table = flatten_objects::FlattenObjects::new();
        fd_table
            .add_at(0, FileDescriptor::new(Arc::new(stdin()), false))
            .unwrap_or_else(|_| panic!()); // stdin
        fd_table
            .add_at(1, FileDescriptor::new(Arc::new(stdout()), false))
            .unwrap_or_else(|_| panic!()); // stdout
        fd_table
            .add_at(2, FileDescriptor::new(Arc::new(stdout()), false))
            .unwrap_or_else(|_| panic!()); // stderr
        fd_table
    }));
}

fn current_fd_table() -> Arc<Mutex<FlattenObjects<FileDescriptor, AX_FILE_LIMIT>>> {
    FD_TABLE.clone_current()
}

pub fn get_file_like(fd: c_int) -> PosixResult<Arc<dyn FileLike>> {
    current_fd_table()
        .lock()
        .get(fd as usize)
        .map(|entry| Arc::clone(&entry.file))
        .ok_or(PosixError::EBADF)
}

#[cfg(any(
    feature = "fs",
    feature = "net",
    feature = "epoll",
    feature = "eventfd"
))]
pub fn add_file_like(f: Arc<dyn FileLike>) -> PosixResult<c_int> {
    Ok(current_fd_table()
        .lock()
        .add(FileDescriptor::new(f, false))
        .map_err(|_| PosixError::EMFILE)? as c_int)
}

/// Installs both endpoints with their descriptor flags under one table lock.
#[cfg(feature = "pipe")]
pub(super) fn add_file_like_pair(
    read_end: Arc<dyn FileLike>,
    write_end: Arc<dyn FileLike>,
    close_on_exec: bool,
) -> PosixResult<[c_int; 2]> {
    let table = current_fd_table();
    let mut table = table.lock();
    let read_fd = match table.add(FileDescriptor::new(read_end, close_on_exec)) {
        Ok(fd) => fd,
        Err(entry) => {
            drop(table);
            drop(entry);
            return Err(PosixError::EMFILE);
        }
    };
    match table.add(FileDescriptor::new(write_end, close_on_exec)) {
        Ok(write_fd) => Ok([read_fd as c_int, write_fd as c_int]),
        Err(write_entry) => {
            let read_entry = table.remove(read_fd).expect("new pipe endpoint must exist");
            drop(table);
            drop(read_entry);
            drop(write_entry);
            Err(PosixError::EMFILE)
        }
    }
}

pub fn close_file_like(fd: c_int) -> PosixResult {
    let f = current_fd_table()
        .lock()
        .remove(fd as usize)
        .ok_or(PosixError::EBADF)?;
    drop(f);
    Ok(())
}

/// Close a file by `fd`.
pub fn sys_close(fd: c_int) -> c_int {
    debug!("sys_close <= {fd}");
    if (0..=2).contains(&fd) {
        return 0; // stdin, stdout, stderr
    }
    syscall_body!(sys_close, close_file_like(fd).map(|_| 0))
}

fn dup_fd(old_fd: c_int, minimum: usize, close_on_exec: bool) -> PosixResult<c_int> {
    let table = current_fd_table();
    let mut table = table.lock();
    let file = Arc::clone(&table.get(old_fd as usize).ok_or(PosixError::EBADF)?.file);
    if minimum >= AX_FILE_LIMIT {
        return Err(PosixError::EINVAL);
    }
    let fd = (minimum..AX_FILE_LIMIT)
        .find(|fd| table.get(*fd).is_none())
        .ok_or(PosixError::EMFILE)?;
    table
        .add_at(fd, FileDescriptor::new(file, close_on_exec))
        .unwrap_or_else(|_| panic!("vacant fd must remain free under the table lock"));
    Ok(fd as c_int)
}

/// Duplicate a file descriptor.
pub fn sys_dup(old_fd: c_int) -> c_int {
    debug!("sys_dup <= {old_fd}");
    syscall_body!(sys_dup, dup_fd(old_fd, 0, false))
}

/// Duplicate a file descriptor, but it uses the file descriptor number specified in `new_fd`.
///
/// TODO: `dup2` should forcibly close new_fd if it is already opened.
pub fn sys_dup2(old_fd: c_int, new_fd: c_int) -> c_int {
    debug!("sys_dup2 <= old_fd: {old_fd}, new_fd: {new_fd}");
    syscall_body!(sys_dup2, {
        if old_fd == new_fd {
            let r = sys_fcntl(old_fd, ctypes::F_GETFD as _, 0);
            if r >= 0 {
                return Ok(old_fd);
            } else {
                return Ok(r);
            }
        }
        if new_fd as usize >= AX_FILE_LIMIT {
            return Err(PosixError::EBADF);
        }

        let f = get_file_like(old_fd)?;
        current_fd_table()
            .lock()
            .add_at(new_fd as usize, FileDescriptor::new(f, false))
            .map_err(|_| PosixError::EMFILE)?;

        Ok(new_fd)
    })
}

/// Manipulate file descriptor.
///
/// Descriptor flags belong to each fd, independently of the shared file.
pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    debug!("sys_fcntl <= fd: {fd} cmd: {cmd} arg: {arg}");
    syscall_body!(sys_fcntl, {
        #[allow(unreachable_patterns)]
        match cmd as u32 {
            ctypes::F_GETFD => {
                let table = current_fd_table();
                let table = table.lock();
                let entry = table.get(fd as usize).ok_or(PosixError::EBADF)?;
                Ok(if entry.close_on_exec {
                    ctypes::FD_CLOEXEC as c_int
                } else {
                    0
                })
            }
            ctypes::F_SETFD => {
                let table = current_fd_table();
                let mut table = table.lock();
                let entry = table.get_mut(fd as usize).ok_or(PosixError::EBADF)?;
                entry.close_on_exec = arg & ctypes::FD_CLOEXEC as usize != 0;
                Ok(0)
            }
            ctypes::F_DUPFD => dup_fd(fd, arg, false),
            ctypes::F_DUPFD_CLOEXEC => dup_fd(fd, arg, true),
            ctypes::F_SETFL => {
                if fd == 0 || fd == 1 || fd == 2 {
                    return Ok(0);
                }
                get_file_like(fd)?.set_nonblocking(arg & (ctypes::O_NONBLOCK as usize) > 0)?;
                Ok(0)
            }
            _ => {
                warn!("unsupported fcntl parameters: cmd {cmd}");
                Ok(0)
            }
        }
    })
}
