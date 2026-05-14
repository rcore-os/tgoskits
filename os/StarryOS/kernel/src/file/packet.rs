use alloc::{borrow::Cow, format, sync::Arc, vec, vec::Vec};
use core::{
    ffi::c_int,
    mem::{MaybeUninit, size_of},
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_io::prelude::*;
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    ioctl::{SIOCGIFFLAGS, SIOCGIFHWADDR, SIOCGIFINDEX},
    net::{AF_PACKET, sockaddr},
};
use starry_vm::{vm_read_slice, vm_write_slice};

use super::{FileLike, Kstat};
use crate::file::{IoDst, IoSrc, get_file_like};

pub(super) const ETH0_IFINDEX: i32 = 2;
const ETH0_NAME: &[u8] = b"eth0";
pub(super) const ETH0_HWADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const SYNTHETIC_PEER_HWADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
const UNSPEC_IPV4: [u8; 4] = [0, 0, 0, 0];

const ARPHRD_ETHER: u16 = 1;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_ARP: u16 = 0x0806;
const IFF_UP: i16 = 0x0001;
const IFF_BROADCAST: i16 = 0x0002;
const IFF_RUNNING: i16 = 0x0040;
const IFF_MULTICAST: i16 = 0x1000;
const ARPOP_REQUEST: u16 = 1;
const ARPOP_REPLY: u16 = 2;
const PACKET_HOST: u8 = 0;
const IFREQ_NAME_LEN: usize = 16;
const IFREQ_DATA_OFFSET: usize = 16;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockAddrLl {
    pub sll_family: u16,
    pub sll_protocol: u16,
    pub sll_ifindex: i32,
    pub sll_hatype: u16,
    pub sll_pkttype: u8,
    pub sll_halen: u8,
    pub sll_addr: [u8; 8],
}

impl SockAddrLl {
    fn eth0(protocol: u16) -> Self {
        let mut sll_addr = [0; 8];
        sll_addr[..ETH0_HWADDR.len()].copy_from_slice(&ETH0_HWADDR);
        Self {
            sll_family: AF_PACKET as u16,
            sll_protocol: protocol,
            sll_ifindex: ETH0_IFINDEX,
            sll_hatype: ARPHRD_ETHER,
            sll_pkttype: PACKET_HOST,
            sll_halen: ETH0_HWADDR.len() as u8,
            sll_addr,
        }
    }

    pub fn read_from_user(addr: *const sockaddr, addrlen: u32) -> AxResult<Self> {
        if addrlen < size_of::<Self>() as u32 {
            return Err(AxError::InvalidInput);
        }
        let data = read_user_bytes::<{ size_of::<Self>() }>(addr as *const u8)?;
        let addr = Self {
            sll_family: u16::from_ne_bytes(data[0..2].try_into().unwrap()),
            sll_protocol: u16::from_ne_bytes(data[2..4].try_into().unwrap()),
            sll_ifindex: i32::from_ne_bytes(data[4..8].try_into().unwrap()),
            sll_hatype: u16::from_ne_bytes(data[8..10].try_into().unwrap()),
            sll_pkttype: data[10],
            sll_halen: data[11],
            sll_addr: data[12..20].try_into().unwrap(),
        };
        if addr.sll_family as u32 != AF_PACKET {
            return Err(AxError::from(LinuxError::EAFNOSUPPORT));
        }
        Ok(addr)
    }

    pub fn write_to_user(&self, addr: *mut sockaddr, addrlen: &mut u32) -> AxResult<()> {
        let len = (*addrlen as usize).min(size_of::<Self>());
        let data = unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, len) };
        vm_write_slice(addr as *mut u8, data)?;
        *addrlen = size_of::<Self>() as u32;
        Ok(())
    }
}

struct PacketFrame {
    data: Vec<u8>,
    from: SockAddrLl,
}

struct PacketSocketState {
    bound: SockAddrLl,
    pending: Option<PacketFrame>,
}

pub struct PacketSocket {
    state: Mutex<PacketSocketState>,
    non_blocking: AtomicBool,
    poll_rx: PollSet,
}

impl PacketSocket {
    pub fn new(protocol: u16) -> Self {
        Self {
            state: Mutex::new(PacketSocketState {
                bound: SockAddrLl::eth0(protocol),
                pending: None,
            }),
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        }
    }

    pub fn bind_ll(&self, addr: SockAddrLl) -> AxResult<()> {
        if addr.sll_ifindex != 0 && addr.sll_ifindex != ETH0_IFINDEX {
            return Err(AxError::NoSuchDevice);
        }
        let mut state = self.state.lock();
        state.bound.sll_family = AF_PACKET as u16;
        state.bound.sll_protocol = addr.sll_protocol;
        state.bound.sll_ifindex = ETH0_IFINDEX;
        if state.bound.sll_halen == 0 {
            state.bound = SockAddrLl::eth0(addr.sll_protocol);
        }
        Ok(())
    }

    pub fn local_addr(&self) -> SockAddrLl {
        self.state.lock().bound
    }

    pub fn send_packet(&self, src: &mut IoSrc) -> AxResult<usize> {
        let len = src.remaining();
        if len == 0 {
            return Ok(0);
        }
        let mut data = vec![0; len];
        let read = src.read(&mut data)?;
        data.truncate(read);

        if let Some(reply) = build_arp_reply(&data) {
            self.state.lock().pending = Some(reply);
            self.poll_rx.wake();
        }
        Ok(read)
    }

    pub fn recv_packet(&self, dst: &mut IoDst) -> AxResult<(usize, SockAddrLl)> {
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let Some(frame) = self.state.lock().pending.take() else {
                return Err(AxError::WouldBlock);
            };
            let written = dst.write(&frame.data)?;
            Ok((written, frame.from))
        }))
    }

    pub fn from_fd(fd: c_int) -> AxResult<Arc<Self>> {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| AxError::NotASocket)
    }
}

fn build_arp_reply(request: &[u8]) -> Option<PacketFrame> {
    if request.len() < 28
        || u16::from_be_bytes([request[0], request[1]]) != ARPHRD_ETHER
        || u16::from_be_bytes([request[2], request[3]]) != ETH_P_IP
        || request[4] != ETH0_HWADDR.len() as u8
        || request[5] != 4
        || u16::from_be_bytes([request[6], request[7]]) != ARPOP_REQUEST
    {
        return None;
    }

    let request_sender_protocol: [u8; 4] = request[14..18].try_into().ok()?;
    let request_target_protocol: [u8; 4] = request[24..28].try_into().ok()?;
    let modeled_peer_protocol = configured_peer_ipv4()?;
    if request_target_protocol != modeled_peer_protocol {
        return None;
    }
    if let Some(local_protocol) = configured_eth0_ipv4()
        && request_sender_protocol != local_protocol
        && request_sender_protocol != UNSPEC_IPV4
    {
        return None;
    }

    let mut reply = request.to_vec();
    reply[6..8].copy_from_slice(&ARPOP_REPLY.to_be_bytes());
    reply[8..14].copy_from_slice(&SYNTHETIC_PEER_HWADDR);
    reply[14..18].copy_from_slice(&modeled_peer_protocol);
    reply[18..24].copy_from_slice(&request[8..14]);
    reply[24..28].copy_from_slice(&request_sender_protocol);

    let mut from = SockAddrLl::eth0(ETH_P_ARP.to_be());
    from.sll_addr[..SYNTHETIC_PEER_HWADDR.len()].copy_from_slice(&SYNTHETIC_PEER_HWADDR);

    Some(PacketFrame { data: reply, from })
}

fn configured_eth0_ipv4() -> Option<[u8; 4]> {
    parse_ipv4_addr(option_env!("AX_IP")?)
}

fn configured_peer_ipv4() -> Option<[u8; 4]> {
    parse_ipv4_addr(option_env!("AX_GW")?)
}

fn parse_ipv4_addr(value: &str) -> Option<[u8; 4]> {
    let mut addr = [0; 4];
    let mut parts = value.split('.');
    for octet in &mut addr {
        let part = parts.next()?;
        if part.is_empty() {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(addr)
}

fn read_user_bytes<const N: usize>(ptr: *const u8) -> AxResult<[u8; N]> {
    let mut buf = [MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn ifreq_name_is_eth0(arg: usize) -> AxResult<bool> {
    let name = read_user_bytes::<IFREQ_NAME_LEN>(arg as *const u8)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    Ok(&name[..end] == ETH0_NAME)
}

fn write_ifreq_data(arg: usize, data: &[u8]) -> AxResult<()> {
    Ok(vm_write_slice((arg + IFREQ_DATA_OFFSET) as *mut u8, data)?)
}

impl FileLike for PacketSocket {
    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFSOCK | 0o777u32,
            blksize: 4096,
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        format!("packet:[{}]", self as *const _ as usize).into()
    }

    fn open_flags(&self) -> u32 {
        O_RDWR
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        if !ifreq_name_is_eth0(arg)? {
            return Err(AxError::NoSuchDevice);
        }

        match cmd {
            SIOCGIFINDEX => write_ifreq_data(arg, &ETH0_IFINDEX.to_ne_bytes())?,
            SIOCGIFFLAGS => write_ifreq_data(
                arg,
                &(IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST).to_ne_bytes(),
            )?,
            SIOCGIFHWADDR => {
                let mut hwaddr = [0; 16];
                hwaddr[..2].copy_from_slice(&ARPHRD_ETHER.to_ne_bytes());
                hwaddr[2..2 + ETH0_HWADDR.len()].copy_from_slice(&ETH0_HWADDR);
                write_ifreq_data(arg, &hwaddr)?;
            }
            _ => return Err(AxError::NotATty),
        }

        Ok(0)
    }
}

impl Pollable for PacketSocket {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT;
        events.set(IoEvents::IN, self.state.lock().pending.is_some());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}
