use alloc::{borrow::Cow, collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    net::AF_NETLINK,
    netlink::{NETLINK_KOBJECT_UEVENT, NETLINK_ROUTE, sockaddr_nl},
};
use spin::Mutex;

use super::packet::{ETH0_HWADDR, ETH0_IFINDEX};
use crate::{
    file::{FileLike, IoDst, IoSrc},
    task::AsThread,
};

const NLMSG_DONE: u16 = 3;
const NLM_F_MULTI: u16 = 2;

const RTM_GETLINK: u16 = 18;
const RTM_NEWLINK: u16 = 16;
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
    name: &'static str,
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
    label: &'static str,
    prefix_len: u8,
    scope: u8,
    local: [u8; 4],
    broadcast: Option<[u8; 4]>,
}

const LINKS: &[LinkInfo] = &[
    LinkInfo {
        index: 1,
        name: "lo",
        ty: ARPHRD_LOOPBACK,
        flags: IFF_UP | IFF_LOOPBACK | IFF_RUNNING | IFF_LOWER_UP,
        mtu: 65536,
        qlen: 1000,
        qdisc: "noqueue",
        operstate: IF_OPER_UNKNOWN,
        address: [0; 6],
        broadcast: [0; 6],
    },
    LinkInfo {
        index: ETH0_IFINDEX,
        name: "eth0",
        ty: ARPHRD_ETHER,
        flags: IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST | IFF_LOWER_UP,
        mtu: 1500,
        qlen: 1000,
        qdisc: "mq",
        operstate: IF_OPER_UP,
        address: ETH0_HWADDR,
        broadcast: [0xff; 6],
    },
];

const ADDRS: &[AddrInfo] = &[
    AddrInfo {
        index: 1,
        label: "lo",
        prefix_len: 8,
        scope: RT_SCOPE_HOST,
        local: [127, 0, 0, 1],
        broadcast: None,
    },
    AddrInfo {
        index: ETH0_IFINDEX as u32,
        label: "eth0",
        prefix_len: 24,
        scope: RT_SCOPE_UNIVERSE,
        local: [10, 0, 2, 15],
        broadcast: Some([10, 0, 2, 255]),
    },
];

#[derive(Default)]
struct NetlinkState {
    addr: Option<sockaddr_nl>,
    receive_buffer_size: usize,
    passcred: bool,
    rx: VecDeque<u8>,
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

    fn build_route_response(&self, request: &[u8]) -> AxResult<Vec<u8>> {
        if request.len() < size_of::<NlMsgHdr>() {
            return Err(AxError::InvalidInput);
        }

        let header = unsafe { request.as_ptr().cast::<NlMsgHdr>().read_unaligned() };
        let pid = self.local_pid();
        let mut response = Vec::new();
        match header.ty {
            RTM_GETLINK => {
                for link in LINKS {
                    push_link_message(&mut response, header.seq, pid, link);
                }
            }
            RTM_GETADDR => {
                for addr in ADDRS {
                    push_addr_message(&mut response, header.seq, pid, addr);
                }
            }
            _ => {}
        }
        push_done_message(&mut response, header.seq, pid);
        Ok(response)
    }
}

impl FileLike for NetlinkSocket {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        if self.protocol != NETLINK_ROUTE {
            return Err(AxError::WouldBlock);
        }

        let mut state = self.state.lock();
        if state.rx.is_empty() {
            return Err(AxError::WouldBlock);
        }

        let count = dst.remaining_mut().min(state.rx.len());
        for _ in 0..count {
            let byte = state.rx.pop_front().unwrap();
            dst.write(&[byte])?;
        }
        Ok(count)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        if self.protocol != NETLINK_ROUTE {
            return Err(AxError::BadFileDescriptor);
        }

        let size = src.remaining();
        let mut request = vec![0; size];
        src.read(&mut request)?;

        let response = self.build_route_response(&request)?;
        let mut state = self.state.lock();
        state.rx.clear();
        state.rx.extend(response);
        drop(state);
        self.poll_rx.wake();
        Ok(size)
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
        if self.protocol != NETLINK_ROUTE {
            return IoEvents::empty();
        }

        let mut events = IoEvents::OUT;
        events.set(IoEvents::IN, !self.state.lock().rx.is_empty());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
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
    push_attr_string(&mut body, IFLA_IFNAME, link.name);
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
    push_attr_string(&mut body, IFA_LABEL, addr.label);
    if let Some(broadcast) = addr.broadcast {
        push_attr(&mut body, IFA_BROADCAST, &broadcast);
    }

    push_nl_header(out, RTM_NEWADDR, NLM_F_MULTI, seq, pid, body.len());
    out.extend_from_slice(&body);
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
