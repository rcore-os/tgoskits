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
    format,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    mem::size_of,
    net::Ipv4Addr,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoIrq as Mutex;
use ax_net::{InterfaceFlags, InterfaceInfo, InterfaceKind};
use ax_task::future::{block_on, poll_io};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    net::AF_NETLINK,
    netlink::{NETLINK_GENERIC, NETLINK_KOBJECT_UEVENT, NETLINK_ROUTE, sockaddr_nl},
};
use spin::LazyLock;

use crate::{
    file::{FileLike, IoDst, IoSrc},
    syscall::in_root_net_ns,
    task::AsThread,
};

/// Maximum number of queued receive messages per socket.  Matches
/// libudev's default monitor buffer expectation (~32 messages × 4 KiB).
const MAX_QUEUED: usize = 128;

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_MULTI: u16 = 2;
const NLM_F_ACK: u16 = 4;

/// Generic netlink controller family ID. Linux's
/// `Documentation/netlink/genetlink-legacy.rst` reserves this for
/// the controller and only the controller; family ID assignment for
/// other families starts above this.
const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_NEWFAMILY: u8 = 1;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const CTRL_ATTR_VERSION: u16 = 3;
const CTRL_ATTR_HDRSIZE: u16 = 4;
const CTRL_ATTR_MAXATTR: u16 = 5;
/// Linux's max length of a family name, including the NUL terminator.
const GENL_NAMSIZ: usize = 16;
const CTRL_VERSION: u32 = 2;
const CTRL_MAX_ATTR: u32 = 11;

const RTM_GETLINK: u16 = 18;
const RTM_NEWLINK: u16 = 16;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWADDR: u16 = 20;

const AF_INET: u8 = 2;
const ARPHRD_ETHER: u16 = 1;
const ARPHRD_LOOPBACK: u16 = 772;

const IFF_UP: u32 = 1;
const IFF_BROADCAST: u32 = 2;
const IFF_LOOPBACK: u32 = 8;
const IFF_RUNNING: u32 = 64;
const IFF_MULTICAST: u32 = 4096;
const IFF_LOWER_UP: u32 = 65536;

const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_QDISC: u16 = 6;
const IFLA_TXQLEN: u16 = 13;
const IFLA_OPERSTATE: u16 = 16;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;

const IF_OPER_UNKNOWN: u8 = 0;
const IF_OPER_UP: u8 = 6;

const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_HOST: u8 = 254;

#[repr(C)]
#[derive(Clone, Copy)]
struct NlMsgHdr {
    len: u32,
    ty: u16,
    flags: u16,
    seq: u32,
    pid: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GenlMsgHdr {
    cmd: u8,
    version: u8,
    reserved: u16,
}

/// Linux errno values used in `NLMSG_ERROR` payloads. Spelled out
/// here so the genl controller doesn't depend on a target-specific
/// errno table.
#[allow(non_upper_case_globals)]
const libc_ENOENT: i32 = 2;
#[allow(non_upper_case_globals)]
const libc_EOPNOTSUPP: i32 = 95;

#[repr(C)]
#[derive(Clone, Copy)]
struct RtAttr {
    len: u16,
    ty: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IfInfoMsg {
    family: u8,
    pad: u8,
    ty: u16,
    index: i32,
    flags: u32,
    change: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IfAddrMsg {
    family: u8,
    prefix_len: u8,
    flags: u8,
    scope: u8,
    index: u32,
}

struct LinkInfo {
    index: i32,
    name: String,
    ty: u16,
    flags: u32,
    mtu: u32,
    qlen: u32,
    qdisc: &'static str,
    operstate: u8,
    address: [u8; 6],
    broadcast: [u8; 6],
}

struct AddrInfo {
    index: u32,
    label: String,
    prefix_len: u8,
    scope: u8,
    local: [u8; 4],
    broadcast: Option<[u8; 4]>,
}

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

/// Global registry of bound netlink sockets, used by [`broadcast`] to dispatch
/// kernel-side messages. Holds weak refs so socket close drops naturally; dead
/// entries are pruned on each broadcast.
static NETLINK_SOCKETS: LazyLock<Mutex<Vec<Weak<NetlinkSocket>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

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

    fn local_pid(&self) -> u32 {
        let mut state = self.state.lock();
        match state.addr {
            Some(addr) if addr.nl_pid != 0 => addr.nl_pid,
            _ => {
                let pid = ax_task::current().as_thread().proc_data.proc.pid();
                state.addr = Some(sockaddr_nl {
                    nl_family: AF_NETLINK as _,
                    nl_pad: 0,
                    nl_pid: pid,
                    nl_groups: 0,
                });
                pid
            }
        }
    }

    /// Minimal NETLINK_GENERIC controller responder. Recognizes
    /// `CTRL_CMD_GETFAMILY` on the controller family (`GENL_ID_CTRL`)
    /// and either reports the controller itself (for a `nlctrl` name
    /// query or a `NLM_F_DUMP`) or returns `NLMSG_ERROR(-ENOENT)`
    /// for any other family name. Any request whose `nlmsg_type` is
    /// not `GENL_ID_CTRL` — i.e. addressed to an unregistered family
    /// — also returns `-ENOENT`. This matches what libnl-genl and
    /// `genl-ctrl-list` need to enumerate the controller and report
    /// "no other families" cleanly.
    fn build_genl_response(&self, request: &[u8]) -> AxResult<Vec<u8>> {
        if request.len() < size_of::<NlMsgHdr>() + size_of::<GenlMsgHdr>() {
            return Err(AxError::InvalidInput);
        }
        let header = unsafe { request.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
        let genl = unsafe {
            request
                .as_ptr()
                .add(size_of::<NlMsgHdr>())
                .cast::<GenlMsgHdr>()
                .read_unaligned()
        };
        let pid = self.local_pid();
        let mut response = Vec::new();

        // Unknown family (anything not the controller) → ENOENT.
        if header.ty != GENL_ID_CTRL {
            push_nlmsg_error(&mut response, request, pid, -libc_ENOENT);
            return Ok(response);
        }

        if genl.cmd != CTRL_CMD_GETFAMILY {
            // Other controller commands are unimplemented.
            push_nlmsg_error(&mut response, request, pid, -libc_EOPNOTSUPP);
            return Ok(response);
        }

        // Parse attributes to see whether the caller asked for a
        // specific family by name. NLM_F_DUMP omits the name and
        // expects all families back — we only have the controller.
        let attrs_start = size_of::<NlMsgHdr>() + size_of::<GenlMsgHdr>();
        let want_name = parse_genl_family_name(&request[attrs_start..]);

        let target_is_ctrl = match want_name.as_deref() {
            None => true, // dump
            Some(name) => name == "nlctrl",
        };

        if !target_is_ctrl {
            push_nlmsg_error(&mut response, request, pid, -libc_ENOENT);
            return Ok(response);
        }

        let is_dump = want_name.is_none();
        push_ctrl_family(&mut response, header.seq, pid, is_dump);
        if is_dump {
            push_done_message(&mut response, header.seq, pid);
        }
        Ok(response)
    }

    fn build_route_response(&self, request: &[u8]) -> AxResult<Vec<u8>> {
        if request.len() < size_of::<NlMsgHdr>() {
            return Err(AxError::InvalidInput);
        }

        let header = unsafe { request.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
        let pid = self.local_pid();
        let in_root = in_root_net_ns();
        let mut response = Vec::new();
        match header.ty {
            RTM_NEWLINK => {
                push_route_update_ack(&mut response, request, pid, handle_newlink(request));
            }
            RTM_NEWADDR => {
                push_route_update_ack(&mut response, request, pid, handle_newaddr(request));
            }
            RTM_DELADDR => {
                push_route_update_ack(&mut response, request, pid, handle_deladdr(request));
            }
            RTM_GETLINK => {
                for link in link_infos() {
                    if !in_root && link.index != 1 {
                        continue;
                    }
                    push_link_message(&mut response, header.seq, pid, &link);
                }
            }
            RTM_GETADDR => {
                for addr in addr_infos() {
                    if !in_root && addr.index != 1 {
                        continue;
                    }
                    push_addr_message(&mut response, header.seq, pid, &addr);
                }
            }
            _ => {
                if header.flags & NLM_F_ACK != 0 {
                    push_nlmsg_error(
                        &mut response,
                        request,
                        pid,
                        -(LinuxError::EOPNOTSUPP.code()),
                    );
                }
            }
        }
        if matches!(header.ty, RTM_GETLINK | RTM_GETADDR) {
            push_done_message(&mut response, header.seq, pid);
        }
        Ok(response)
    }

    /// Drain at most one queued message into `dst`.  Returns `WouldBlock`
    /// when the queue is empty.
    ///
    /// `peek` (MSG_PEEK): copy the front message into `dst` without removing it
    /// from the queue, so a following non-peek recv reads the same message.
    /// glibc/musl `getifaddrs()` and dnsmasq size their buffer with a
    /// `MSG_PEEK|MSG_TRUNC` recv first, then read for real; popping on peek
    /// loses the dump and the second recv blocks forever (startup stall).
    ///
    /// `truncate` (MSG_TRUNC): netlink datagrams are not coalesced, so a short
    /// buffer drops the tail. Linux returns the *real* datagram length (not the
    /// copied length) so the caller can resize and retry.
    /// Returns `(len, truncated)`: `len` is the full datagram length when
    /// `truncate` (MSG_TRUNC) is set, else the number of bytes copied;
    /// `truncated` is true when the datagram did not fit in `dst` (so the
    /// caller can raise MSG_TRUNC in `msg_flags`).
    fn read_one(&self, dst: &mut IoDst, peek: bool, truncate: bool) -> AxResult<(usize, bool)> {
        let msg = {
            let mut queue = self.queue.lock();
            if peek {
                let Some(msg) = queue.front() else {
                    return Err(AxError::WouldBlock);
                };
                msg.clone()
            } else {
                let Some(msg) = queue.pop_front() else {
                    return Err(AxError::WouldBlock);
                };
                msg
            }
        };
        // Cap at the message length; netlink datagrams are not coalesced.
        let full = msg.len();
        let copied = dst.write(&msg)?;
        let truncated = full > copied;
        Ok((if truncate { full } else { copied }, truncated))
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
            // Netlink message is queued before waking readers.
            unsafe { sock.poll_rx.wake(IoEvents::IN) };
        }
    }
}

impl NetlinkSocket {
    /// Flag-aware receive used by `recvmsg`/`recvfrom`. Honors MSG_PEEK (do not
    /// consume), MSG_TRUNC (return full datagram length), and MSG_DONTWAIT
    /// (per-call non-blocking).
    pub fn recv(
        &self,
        dst: &mut IoDst,
        peek: bool,
        truncate: bool,
        dontwait: bool,
    ) -> AxResult<(usize, bool)> {
        let non_blocking = self.nonblocking() || dontwait;
        block_on(poll_io(self, IoEvents::IN, non_blocking, || {
            self.read_one(dst, peek, truncate)
        }))
    }
}

impl FileLike for NetlinkSocket {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            self.read_one(dst, false, false)
        }))
        .map(|(len, _)| len)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        let size = src.remaining().min(64 * 1024);
        let mut request = vec![0; size];
        let total = src.read(&mut request)?;
        request.truncate(total);

        let response = match self.protocol {
            NETLINK_ROUTE => Some(self.build_route_response(&request)?),
            NETLINK_GENERIC => Some(self.build_genl_response(&request)?),
            _ => None,
        };
        if let Some(response) = response {
            let mut queue = self.queue.lock();
            // Append the protocol reply alongside whatever async
            // broadcasts (uevent, rtnetlink events) `broadcast()` may
            // have already pushed onto this socket. Earlier revisions
            // cleared the queue here, which let a request/response
            // round-trip silently drop queued events the user-space
            // listener had not yet drained — wrong for a single fd
            // that is shared between event subscription and direct
            // queries. Linux's netlink only drops on bounded
            // backpressure, never as a side effect of `send_to`.
            if queue.len() < MAX_QUEUED {
                queue.push_back(response);
                drop(queue);
                // Netlink response is queued before waking readers.
                unsafe { self.poll_rx.wake(IoEvents::IN) };
            }
        }
        Ok(total)
    }

    fn stat(&self) -> AxResult<crate::file::Kstat> {
        Ok(crate::file::Kstat {
            mode: S_IFSOCK | 0o777,
            blksize: 4096,
            ..Default::default()
        })
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        if self.protocol == NETLINK_KOBJECT_UEVENT {
            "socket:[netlink]".into()
        } else {
            format!("netlink:{}:[{}]", self.protocol, self as *const _ as usize).into()
        }
    }

    fn open_flags(&self) -> u32 {
        O_RDWR
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
            // Registration happens from socket poll task context.
            unsafe { self.poll_rx.register(context.waker(), IoEvents::IN) };
        }
    }
}

fn link_infos() -> Vec<LinkInfo> {
    ax_net::interfaces()
        .into_iter()
        .map(|info| {
            let flags = linux_link_flags(&info);
            let mut address = [0; 6];
            if let Some(mac) = info.mac {
                address = mac.0;
            }
            LinkInfo {
                index: info.id.get() as i32,
                name: info.name,
                ty: match info.kind {
                    InterfaceKind::Loopback => ARPHRD_LOOPBACK,
                    InterfaceKind::Ethernet => ARPHRD_ETHER,
                },
                flags,
                mtu: info.mtu as u32,
                qlen: 1000,
                qdisc: match info.kind {
                    InterfaceKind::Loopback => "noqueue",
                    InterfaceKind::Ethernet => "mq",
                },
                operstate: if info.flags.contains(InterfaceFlags::RUNNING) {
                    IF_OPER_UP
                } else {
                    IF_OPER_UNKNOWN
                },
                address,
                broadcast: if info.kind == InterfaceKind::Ethernet {
                    [0xff; 6]
                } else {
                    [0; 6]
                },
            }
        })
        .collect()
}

fn addr_infos() -> Vec<AddrInfo> {
    ax_net::interfaces()
        .into_iter()
        .filter_map(|info| {
            let ipv4 = info.ipv4?;
            let local = ipv4.address.address().octets();
            let broadcast = (info.kind == InterfaceKind::Ethernet).then(|| {
                let ip = u32::from_be_bytes(local);
                let mask = if ipv4.address.prefix_len() == 0 {
                    0
                } else {
                    !0u32 << (32 - ipv4.address.prefix_len())
                };
                (ip | !mask).to_be_bytes()
            });
            Some(AddrInfo {
                index: info.id.get(),
                label: info.name,
                prefix_len: ipv4.address.prefix_len(),
                scope: match info.kind {
                    InterfaceKind::Loopback => RT_SCOPE_HOST,
                    InterfaceKind::Ethernet => RT_SCOPE_UNIVERSE,
                },
                local,
                broadcast,
            })
        })
        .collect()
}

fn linux_link_flags(info: &InterfaceInfo) -> u32 {
    let mut flags = 0;
    if info.flags.contains(InterfaceFlags::UP) {
        flags |= IFF_UP;
    }
    if info.flags.contains(InterfaceFlags::BROADCAST) {
        flags |= IFF_BROADCAST;
    }
    if info.flags.contains(InterfaceFlags::LOOPBACK) {
        flags |= IFF_LOOPBACK;
    }
    if info.flags.contains(InterfaceFlags::RUNNING) {
        flags |= IFF_RUNNING | IFF_LOWER_UP;
    }
    if info.flags.contains(InterfaceFlags::MULTICAST) {
        flags |= IFF_MULTICAST;
    }
    flags
}

fn push_link_message(out: &mut Vec<u8>, seq: u32, pid: u32, link: &LinkInfo) {
    let mut body = Vec::new();
    push_struct(
        &mut body,
        &IfInfoMsg {
            family: 0,
            pad: 0,
            ty: link.ty,
            index: link.index,
            flags: link.flags,
            change: 0,
        },
    );
    push_attr_string(&mut body, IFLA_IFNAME, &link.name);
    push_attr(&mut body, IFLA_ADDRESS, &link.address);
    push_attr(&mut body, IFLA_BROADCAST, &link.broadcast);
    push_attr(&mut body, IFLA_MTU, &link.mtu.to_ne_bytes());
    push_attr(&mut body, IFLA_QDISC, link.qdisc.as_bytes());
    push_attr(&mut body, IFLA_TXQLEN, &link.qlen.to_ne_bytes());
    push_attr(&mut body, IFLA_OPERSTATE, &[link.operstate]);

    push_nl_header(out, RTM_NEWLINK, NLM_F_MULTI, seq, pid, body.len());
    out.extend_from_slice(&body);
}

fn push_addr_message(out: &mut Vec<u8>, seq: u32, pid: u32, addr: &AddrInfo) {
    let mut body = Vec::new();
    push_struct(
        &mut body,
        &IfAddrMsg {
            family: AF_INET,
            prefix_len: addr.prefix_len,
            flags: 0,
            scope: addr.scope,
            index: addr.index,
        },
    );
    push_attr(&mut body, IFA_ADDRESS, &addr.local);
    push_attr(&mut body, IFA_LOCAL, &addr.local);
    push_attr_string(&mut body, IFA_LABEL, &addr.label);
    if let Some(broadcast) = addr.broadcast {
        push_attr(&mut body, IFA_BROADCAST, &broadcast);
    }

    push_nl_header(out, RTM_NEWADDR, NLM_F_MULTI, seq, pid, body.len());
    out.extend_from_slice(&body);
}

fn handle_newlink(request: &[u8]) -> Result<(), LinuxError> {
    let header_len = size_of::<NlMsgHdr>();
    if request.len() < header_len + size_of::<IfInfoMsg>() {
        return Err(LinuxError::EINVAL);
    }
    let msg = unsafe {
        request
            .as_ptr()
            .add(header_len)
            .cast::<IfInfoMsg>()
            .read_unaligned()
    };
    let Some(id) = ax_net::InterfaceId::from_linux_ifindex(msg.index) else {
        return Err(LinuxError::ENODEV);
    };
    let info = ax_net::interface_by_id(id).ok_or(LinuxError::ENODEV)?;
    if !in_root_net_ns() && info.kind != ax_net::InterfaceKind::Loopback {
        return Err(LinuxError::EACCES);
    }
    if msg.change & IFF_UP != 0 {
        ax_net::set_interface_up(&info.name, msg.flags & IFF_UP != 0).map_err(LinuxError::from)?;
    }
    Ok(())
}

fn handle_newaddr(request: &[u8]) -> Result<(), LinuxError> {
    let header_len = size_of::<NlMsgHdr>();
    if request.len() < header_len + size_of::<IfAddrMsg>() {
        return Err(LinuxError::EINVAL);
    }
    let msg = unsafe {
        request
            .as_ptr()
            .add(header_len)
            .cast::<IfAddrMsg>()
            .read_unaligned()
    };
    if msg.family != AF_INET || msg.prefix_len > 32 {
        return Err(LinuxError::EINVAL);
    }
    let id = ax_net::InterfaceId::from_linux_ifindex(msg.index as i32).ok_or(LinuxError::ENODEV)?;
    let info = ax_net::interface_by_id(id).ok_or(LinuxError::ENODEV)?;
    if !in_root_net_ns() && info.kind != ax_net::InterfaceKind::Loopback {
        return Err(LinuxError::EACCES);
    }
    let attrs = route_attrs(request, header_len + size_of::<IfAddrMsg>())?;
    let address = attr_ipv4(&attrs, IFA_LOCAL)
        .or_else(|| attr_ipv4(&attrs, IFA_ADDRESS))
        .ok_or(LinuxError::EINVAL)?;
    let gateway = info
        .ipv4
        .and_then(|ipv4| ipv4.gateway.map(|gateway| Ipv4Addr::from(gateway.octets())));
    ax_net::configure_ipv4_addr(&info.name, Some((address, msg.prefix_len)), gateway)
        .map_err(LinuxError::from)
}

fn handle_deladdr(request: &[u8]) -> Result<(), LinuxError> {
    let header_len = size_of::<NlMsgHdr>();
    if request.len() < header_len + size_of::<IfAddrMsg>() {
        return Err(LinuxError::EINVAL);
    }
    let msg = unsafe {
        request
            .as_ptr()
            .add(header_len)
            .cast::<IfAddrMsg>()
            .read_unaligned()
    };
    if msg.family != AF_INET {
        return Err(LinuxError::EINVAL);
    }
    let id = ax_net::InterfaceId::from_linux_ifindex(msg.index as i32).ok_or(LinuxError::ENODEV)?;
    let info = ax_net::interface_by_id(id).ok_or(LinuxError::ENODEV)?;
    if !in_root_net_ns() && info.kind != ax_net::InterfaceKind::Loopback {
        return Err(LinuxError::EACCES);
    }
    let attrs = route_attrs(request, header_len + size_of::<IfAddrMsg>())?;
    let requested = attr_ipv4(&attrs, IFA_LOCAL).or_else(|| attr_ipv4(&attrs, IFA_ADDRESS));
    if let (Some(current), Some(requested)) = (
        info.ipv4
            .map(|ipv4| Ipv4Addr::from(ipv4.address.address().octets())),
        requested,
    ) && current != requested
    {
        return Err(LinuxError::EADDRNOTAVAIL);
    }
    ax_net::configure_ipv4_addr(&info.name, None, None).map_err(LinuxError::from)
}

fn push_route_update_ack(
    out: &mut Vec<u8>,
    request: &[u8],
    pid: u32,
    result: Result<(), LinuxError>,
) {
    let err = result.err().map_or(0, |err| -err.code());
    push_nlmsg_error(out, request, pid, err);
}

fn route_attrs(request: &[u8], start: usize) -> Result<Vec<(u16, Vec<u8>)>, LinuxError> {
    let header = unsafe { request.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
    let end = (header.len as usize).min(request.len());
    if start > end {
        return Err(LinuxError::EINVAL);
    }
    parse_route_attrs(&request[start..end])
}

fn parse_route_attrs(mut buf: &[u8]) -> Result<Vec<(u16, Vec<u8>)>, LinuxError> {
    let mut attrs = Vec::new();
    while buf.len() >= size_of::<RtAttr>() {
        let attr = unsafe { buf.as_ptr().cast::<RtAttr>().read_unaligned() };
        let len = attr.len as usize;
        if len < size_of::<RtAttr>() || len > buf.len() {
            return Err(LinuxError::EINVAL);
        }
        attrs.push((attr.ty, buf[size_of::<RtAttr>()..len].to_vec()));
        let aligned = (len + 3) & !3;
        if aligned > buf.len() {
            return Err(LinuxError::EINVAL);
        }
        buf = &buf[aligned..];
    }
    Ok(attrs)
}

fn attr_ipv4(attrs: &[(u16, Vec<u8>)], ty: u16) -> Option<Ipv4Addr> {
    attrs
        .iter()
        .find(|(attr_ty, value)| *attr_ty == ty && value.len() >= 4)
        .map(|(_, value)| Ipv4Addr::new(value[0], value[1], value[2], value[3]))
}

fn push_ctrl_family(out: &mut Vec<u8>, seq: u32, pid: u32, multi: bool) {
    let mut payload = Vec::new();
    push_struct(
        &mut payload,
        &GenlMsgHdr {
            cmd: CTRL_CMD_NEWFAMILY,
            version: CTRL_VERSION as u8,
            reserved: 0,
        },
    );
    push_attr(
        &mut payload,
        CTRL_ATTR_FAMILY_ID,
        &GENL_ID_CTRL.to_ne_bytes(),
    );
    push_attr_string(&mut payload, CTRL_ATTR_FAMILY_NAME, "nlctrl");
    push_attr(&mut payload, CTRL_ATTR_VERSION, &CTRL_VERSION.to_ne_bytes());
    push_attr(&mut payload, CTRL_ATTR_HDRSIZE, &0u32.to_ne_bytes());
    push_attr(
        &mut payload,
        CTRL_ATTR_MAXATTR,
        &CTRL_MAX_ATTR.to_ne_bytes(),
    );

    let flags = if multi { NLM_F_MULTI } else { 0 };
    push_nl_header(out, GENL_ID_CTRL, flags, seq, pid, payload.len());
    out.extend_from_slice(&payload);
}

/// Emit a `NLMSG_ERROR` whose payload echoes the entire original
/// request — `i32 error` followed by the request bytes. Linux's
/// `struct nlmsgerr { int error; struct nlmsghdr msg; }` is followed
/// by the request payload, and libnl `nl_recvmsgs` walks the inner
/// nlmsghdr's `nlmsg_len` to find the end of the error frame. Echoing
/// only the header would leave the inner `nlmsg_len` pointing past
/// the bytes actually written and trip libnl's parser.
fn push_nlmsg_error(out: &mut Vec<u8>, request_bytes: &[u8], pid: u32, error: i32) {
    let header = unsafe { request_bytes.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
    let req_len = (header.len as usize).min(request_bytes.len());
    let payload_len = size_of::<i32>() + req_len;
    push_nl_header(out, NLMSG_ERROR, 0, header.seq, pid, payload_len);
    out.extend_from_slice(&error.to_ne_bytes());
    out.extend_from_slice(&request_bytes[..req_len]);
}

/// Walk the attribute stream after a `genlmsghdr` and return the
/// payload of the first `CTRL_ATTR_FAMILY_NAME` attribute, with the
/// trailing NUL stripped. Returns `None` when no name attribute is
/// present (e.g. a `NLM_F_DUMP` request).
fn parse_genl_family_name(mut buf: &[u8]) -> Option<alloc::string::String> {
    while buf.len() >= size_of::<RtAttr>() {
        let attr = unsafe { buf.as_ptr().cast::<RtAttr>().read_unaligned() };
        let len = attr.len as usize;
        if len < size_of::<RtAttr>() || len > buf.len() {
            return None;
        }
        if attr.ty == CTRL_ATTR_FAMILY_NAME {
            let payload = &buf[size_of::<RtAttr>()..len];
            let nul = payload
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(payload.len());
            let limited = &payload[..nul.min(GENL_NAMSIZ - 1)];
            return core::str::from_utf8(limited)
                .ok()
                .map(alloc::string::String::from);
        }
        let aligned = (len + 3) & !3;
        if aligned > buf.len() {
            return None;
        }
        buf = &buf[aligned..];
    }
    None
}

fn push_done_message(out: &mut Vec<u8>, seq: u32, pid: u32) {
    push_nl_header(out, NLMSG_DONE, NLM_F_MULTI, seq, pid, size_of::<i32>());
    out.extend_from_slice(&0i32.to_ne_bytes());
}

fn push_nl_header(out: &mut Vec<u8>, ty: u16, flags: u16, seq: u32, pid: u32, payload_len: usize) {
    push_struct(
        out,
        &NlMsgHdr {
            len: (size_of::<NlMsgHdr>() + payload_len) as u32,
            ty,
            flags,
            seq,
            pid,
        },
    );
}

fn push_attr_string(out: &mut Vec<u8>, ty: u16, value: &str) {
    let mut data = String::from(value).into_bytes();
    data.push(0);
    push_attr(out, ty, &data);
}

fn push_attr(out: &mut Vec<u8>, ty: u16, payload: &[u8]) {
    let len = size_of::<RtAttr>() + payload.len();
    push_struct(
        out,
        &RtAttr {
            len: len as u16,
            ty,
        },
    );
    out.extend_from_slice(payload);
    pad_to_align4(out);
}

fn push_struct<T>(out: &mut Vec<u8>, value: &T) {
    out.extend_from_slice(unsafe { as_bytes(value) });
}

unsafe fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn pad_to_align4(out: &mut Vec<u8>) {
    let aligned = (out.len() + 3) & !3;
    out.resize(aligned, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_route_attrs_extracts_ipv4_attributes() {
        let mut buf = Vec::new();
        push_attr(&mut buf, IFA_ADDRESS, &[10, 0, 2, 15]);
        push_attr(&mut buf, IFA_LOCAL, &[192, 0, 2, 33]);

        let attrs = parse_route_attrs(&buf).unwrap();

        assert_eq!(
            attr_ipv4(&attrs, IFA_ADDRESS),
            Some(Ipv4Addr::new(10, 0, 2, 15))
        );
        assert_eq!(
            attr_ipv4(&attrs, IFA_LOCAL),
            Some(Ipv4Addr::new(192, 0, 2, 33))
        );
    }

    #[test]
    fn push_route_update_ack_encodes_linux_errno() {
        let mut request = Vec::new();
        push_struct(
            &mut request,
            &NlMsgHdr {
                len: size_of::<NlMsgHdr>() as u32,
                ty: RTM_NEWADDR,
                flags: NLM_F_ACK,
                seq: 7,
                pid: 11,
            },
        );

        let mut response = Vec::new();
        push_route_update_ack(&mut response, &request, 23, Err(LinuxError::EINVAL));

        let header = unsafe { response.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
        let error_offset = size_of::<NlMsgHdr>();
        let error = i32::from_ne_bytes(
            response[error_offset..error_offset + size_of::<i32>()]
                .try_into()
                .unwrap(),
        );

        assert_eq!(header.ty, NLMSG_ERROR);
        assert_eq!(header.seq, 7);
        assert_eq!(header.pid, 23);
        assert_eq!(error, -LinuxError::EINVAL.code());
    }
}
