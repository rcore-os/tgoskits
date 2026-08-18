use alloc::sync::Arc;
use core::ffi::c_int;

use ax_io::PollState;
use ax_runtime::sync::SpinRwLock as RwLock;
use flatten_objects::FlattenObjects;
use scope_local::scope_local;

use crate::{
    PosixError, PosixResult, ctypes,
    imp::stdio::{stdin, stdout},
};

pub const AX_FILE_LIMIT: usize = 1024;

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
    pub(crate) static FD_TABLE: Arc<RwLock<FlattenObjects<Arc<dyn FileLike>, AX_FILE_LIMIT>>> = Arc::new(RwLock::new({
        let mut fd_table = flatten_objects::FlattenObjects::new();
        fd_table
            .add_at(0, Arc::new(stdin()) as _)
            .unwrap_or_else(|_| panic!()); // stdin
        fd_table
            .add_at(1, Arc::new(stdout()) as _)
            .unwrap_or_else(|_| panic!()); // stdout
        fd_table
            .add_at(2, Arc::new(stdout()) as _)
            .unwrap_or_else(|_| panic!()); // stderr
        fd_table
    }));
}

fn current_fd_table() -> Arc<RwLock<FlattenObjects<Arc<dyn FileLike>, AX_FILE_LIMIT>>> {
    FD_TABLE.clone_current()
}

pub fn get_file_like(fd: c_int) -> PosixResult<Arc<dyn FileLike>> {
    current_fd_table()
        .read()
        .get(fd as usize)
        .cloned()
        .ok_or(PosixError::EBADF)
}

pub fn add_file_like(f: Arc<dyn FileLike>) -> PosixResult<c_int> {
    Ok(current_fd_table()
        .write()
        .add(f)
        .map_err(|_| PosixError::EMFILE)? as c_int)
}

pub fn close_file_like(fd: c_int) -> PosixResult {
    let f = current_fd_table()
        .write()
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

fn dup_fd(old_fd: c_int) -> PosixResult<c_int> {
    let f = get_file_like(old_fd)?;
    let new_fd = add_file_like(f)?;
    Ok(new_fd)
}

/// Duplicate a file descriptor.
pub fn sys_dup(old_fd: c_int) -> c_int {
    debug!("sys_dup <= {old_fd}");
    syscall_body!(sys_dup, dup_fd(old_fd))
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
            .write()
            .add_at(new_fd as usize, f)
            .map_err(|_| PosixError::EMFILE)?;

        Ok(new_fd)
    })
}

/// Manipulate file descriptor.
///
/// TODO: `SET/GET` command is ignored, hard-code stdin/stdout
pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    debug!("sys_fcntl <= fd: {fd} cmd: {cmd} arg: {arg}");
    syscall_body!(sys_fcntl, {
        #[allow(unreachable_patterns)]
        match cmd as u32 {
            ctypes::F_DUPFD => dup_fd(fd),
            ctypes::F_DUPFD_CLOEXEC => {
                // TODO: Change fd flags
                dup_fd(fd)
            }
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
