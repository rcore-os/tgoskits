//! `AF_NETLINK` socket family.
//!
//! Minimal but real implementation covering the use cases that matter for
//! libudev / iproute2 / genl-ctrl-list interop:
//!
//! - `NETLINK_KOBJECT_UEVENT` (15): subscribe to kernel uevent broadcasts;
//!   listener side only — kernel emitters call [`broadcast`].
//! - `NETLINK_ROUTE` (0): rtnetlink socket; `bind` + `read`/`recv` work,
//!   actual RTM_GETLINK / RTM_GETADDR responder lives elsewhere (the socket
//!   here just provides the byte transport).
//! - `NETLINK_GENERIC` (16): same shape as NETLINK_ROUTE — a queued byte
//!   transport that genl userspace can drive.
//!
//! The kernel calls [`broadcast`] with a protocol id, a group bit, and a
//! byte payload; every bound socket subscribed to that protocol + group
//! gets the payload pushed into its receive queue and its pollers woken.
//! Queue full → drop (matches Linux `sock_queue_rcv_skb_reason()`).

use alloc::{
    borrow::Cow,
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_task::future::{block_on, poll_io};
use axpoll::{IoEvents, PollSet, Pollable};
use lazy_static::lazy_static;
use linux_raw_sys::{net::AF_NETLINK, netlink::sockaddr_nl};
use spin::Mutex;

use crate::file::{FileLike, IoDst, IoSrc};

/// Maximum number of queued receive messages per socket.  Matches
/// libudev's default monitor buffer expectation (~32 messages × 4 KiB).
const MAX_QUEUED: usize = 128;

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
    queue: Mutex<VecDeque<Vec<u8>>>,
}

lazy_static! {
    /// Global registry of bound netlink sockets, used by [`broadcast`]
    /// to dispatch kernel-side messages.  Holds weak refs so socket close
    /// drops naturally; dead entries are pruned on each broadcast.
    static ref NETLINK_SOCKETS: Mutex<Vec<Weak<NetlinkSocket>>> = Mutex::new(Vec::new());
}

impl NetlinkSocket {
    pub fn new(protocol: u32) -> Arc<Self> {
        Arc::new(Self {
            protocol,
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
            state: Mutex::new(NetlinkState::default()),
            queue: Mutex::new(VecDeque::with_capacity(MAX_QUEUED)),
        })
    }

    pub fn bind(self: &Arc<Self>, addr: sockaddr_nl) -> AxResult {
        if addr.nl_family as u32 != AF_NETLINK {
            return Err(AxError::InvalidInput);
        }
        {
            let mut state = self.state.lock();
            if state.addr.is_some() {
                return Err(AxError::InvalidInput);
            }
            state.addr = Some(addr);
        }
        // Register self in the global broadcast registry so kernel-side
        // `broadcast()` calls can reach this socket.
        NETLINK_SOCKETS.lock().push(Arc::downgrade(self));
        Ok(())
    }

    pub fn local_addr(&self) -> sockaddr_nl {
        self.state.lock().addr.unwrap_or(sockaddr_nl {
            nl_family: AF_NETLINK as _,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        })
    }

    pub fn kernel_addr(&self) -> sockaddr_nl {
        sockaddr_nl {
            nl_family: AF_NETLINK as _,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        }
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

    /// Drain at most one queued message into `dst`.  Returns `WouldBlock`
    /// when the queue is empty.
    fn read_one(&self, dst: &mut IoDst) -> AxResult<usize> {
        let mut queue = self.queue.lock();
        let Some(msg) = queue.pop_front() else {
            return Err(AxError::WouldBlock);
        };
        // Cap at the message length; netlink datagrams are not coalesced.
        let n = dst.write(&msg)?;
        Ok(n)
    }
}

/// Push `payload` onto every currently-bound netlink socket that matches
/// `protocol` and has any of the bits in `group_mask` set in its
/// subscribed-groups bitmask.  Kernel-side broadcast entry point — call
/// this from uevent emission / rtnetlink event generators.
///
/// Silently drops the payload for any socket whose queue is full (same
/// as Linux: the kernel buffer is bounded and consumers that don't drain
/// fast enough lose events, not the producer).
#[allow(dead_code)]
pub fn broadcast(protocol: u32, group_mask: u32, payload: &[u8]) {
    let mut sockets = NETLINK_SOCKETS.lock();
    sockets.retain(|weak| weak.strong_count() > 0);
    for weak in sockets.iter() {
        let Some(sock) = weak.upgrade() else { continue };
        if sock.protocol != protocol {
            continue;
        }
        let subscribed = sock.state.lock().addr.map(|a| a.nl_groups).unwrap_or(0);
        if group_mask != 0 && subscribed & group_mask == 0 {
            continue;
        }
        let mut queue = sock.queue.lock();
        if queue.len() < MAX_QUEUED {
            queue.push_back(payload.to_vec());
            drop(queue);
            sock.poll_rx.wake();
        }
    }
}

impl FileLike for NetlinkSocket {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            self.read_one(dst)
        }))
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        // Drain the user buffer (up to 64 KiB) and report success.
        // Userspace→kernel netlink traffic is not interpreted at this
        // layer; protocol-specific responders (rtnetlink, genl) will
        // intercept on the way in once they exist.  libudev never sends
        // on its monitor socket, so this is a no-op for it today.
        let mut buf = [0u8; 4096];
        let mut total = 0usize;
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total = total.saturating_add(n);
            if total >= 64 * 1024 {
                break;
            }
        }
        Ok(total)
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
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, !self.queue.lock().is_empty());
        events.insert(IoEvents::OUT);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
