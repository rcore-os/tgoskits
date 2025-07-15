use alloc::sync::Arc;
use core::{ffi::c_int, ops::Deref};

use axerrno::{LinuxError, LinuxResult};
use axio::PollState;
use axnet::{
    SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};
use linux_raw_sys::general::S_IFSOCK;

use super::{FileLike, Kstat};
use crate::file::get_file_like;

pub struct Socket(pub axnet::Socket);
impl Deref for Socket {
    type Target = axnet::Socket;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FileLike for Socket {
    fn read(&self, mut buf: &mut [u8]) -> LinuxResult<usize> {
        self.recv(&mut buf, None, axnet::RecvFlags::empty())
    }

    fn write(&self, mut buf: &[u8]) -> LinuxResult<usize> {
        self.send(&mut buf, None, axnet::SendFlags::empty())
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        // TODO(mivik): implement stat for sockets
        Ok(Kstat {
            mode: S_IFSOCK | 0o777u32, // rwxrwxrwx
            blksize: 4096,
            ..Default::default()
        })
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }

    fn poll(&self) -> LinuxResult<PollState> {
        self.0.poll()
    }

    fn nonblocking(&self) -> bool {
        let mut result = false;
        self.get_option(GetSocketOption::NonBlocking(&mut result))
            .unwrap();
        result
    }

    fn set_nonblocking(&self, nonblocking: bool) -> LinuxResult<()> {
        self.0
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
    }

    fn from_fd(fd: c_int) -> LinuxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| LinuxError::ENOTSOCK)
    }
}
