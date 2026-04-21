use alloc::{borrow::Cow, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{net::AF_NETLINK, netlink::sockaddr_nl};
use spin::Mutex;

use crate::file::{FileLike, IoDst, IoSrc};

#[derive(Clone, Copy, Default)]
struct NetlinkState {
    addr: Option<sockaddr_nl>,
    receive_buffer_size: usize,
    passcred: bool,
}

pub struct NetlinkSocket {
    protocol: u32,
    non_blocking: AtomicBool,
    poll_rx: PollSet,
    state: Mutex<NetlinkState>,
}

impl NetlinkSocket {
    pub fn new(protocol: u32) -> Arc<Self> {
        Arc::new(Self {
            protocol,
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
            state: Mutex::new(NetlinkState::default()),
        })
    }

    pub fn bind(&self, addr: sockaddr_nl) -> AxResult {
        if addr.nl_family as u32 != AF_NETLINK {
            return Err(AxError::InvalidInput);
        }
        self.state.lock().addr = Some(addr);
        Ok(())
    }

    pub fn set_receive_buffer_size(&self, size: usize) {
        self.state.lock().receive_buffer_size = size;
    }

    pub fn set_passcred(&self, enabled: bool) {
        self.state.lock().passcred = enabled;
    }

    #[allow(dead_code)]
    pub fn protocol(&self) -> u32 {
        self.protocol
    }
}

impl FileLike for NetlinkSocket {
    fn read(&self, _dst: &mut IoDst) -> AxResult<usize> {
        Err(AxError::WouldBlock)
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::BadFileDescriptor)
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "socket:[netlink]".into()
    }
}

impl Pollable for NetlinkSocket {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
