use alloc::{borrow::Cow, format, sync::Arc};
use core::{ffi::c_int, ops::Deref, task::Context};

use axerrno::{AxError, AxResult};
use axnet::{
    SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::S_IFSOCK;

use super::{FileLike, Kstat};
use crate::file::{SealedBuf, SealedBufMut, get_file_like};

pub struct Socket(pub axnet::Socket);

impl Deref for Socket {
    type Target = axnet::Socket;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FileLike for Socket {
    fn read(&self, dst: &mut SealedBufMut) -> AxResult<usize> {
        self.recv(dst, axnet::RecvOptions::default())
    }

    fn write(&self, src: &mut SealedBuf) -> AxResult<usize> {
        self.send(src, axnet::SendOptions::default())
    }

    fn stat(&self) -> AxResult<Kstat> {
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

    fn nonblocking(&self) -> bool {
        let mut result = false;
        self.get_option(GetSocketOption::NonBlocking(&mut result))
            .unwrap();
        result
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult<()> {
        self.0
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
    }

    fn path(&self) -> Cow<'_, str> {
        format!("socket:[{}]", self as *const _ as usize).into()
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .into_any()
            .downcast::<Self>()
            .map_err(|_| AxError::NotASocket)
    }
}
impl Pollable for Socket {
    fn poll(&self) -> IoEvents {
        self.0.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.0.register(context, events);
    }
}
