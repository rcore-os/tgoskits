use alloc::{borrow::Cow, format, sync::Arc, vec, vec::Vec};
use core::{
    ffi::c_int,
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_io::prelude::*;
use ax_net::{InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind};
use ax_sync::PiMutex;
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    ioctl::{SIOCGIFFLAGS, SIOCGIFHWADDR, SIOCGIFINDEX},
    net::{AF_PACKET, sockaddr},
};
use starry_vm::{vm_read_slice, vm_write_slice};

use super::{
    FileLike, Kstat,
    net::{ARPHRD_ETHER, first_visible_ethernet, visible_interface_by_id},
};
use crate::{
    file::{IoDst, IoSrc, get_file_like},
    syscall::in_root_net_ns,
    task::{
        current_user_task,
        future::{block_on_user, poll_io_for},
    },
};

const PACKET_HOST: u8 = 0;
const SYNTHETIC_PEER_HWADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
const ETH_P_IP: u16 = 0x0800;
const ETH_P_ARP: u16 = 0x0806;
const ARPOP_REQUEST: u16 = 1;
const ARPOP_REPLY: u16 = 2;
const IFF_UP: i16 = 0x0001;
const IFF_BROADCAST: i16 = 0x0002;
const IFF_LOOPBACK: i16 = 0x0008;
const IFF_RUNNING: i16 = 0x0040;
const IFF_MULTICAST: i16 = 0x1000;
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
    fn from_interface(info: &InterfaceInfo, protocol: u16) -> AxResult<Self> {
        if info.kind != InterfaceKind::Ethernet {
            return Err(AxError::NoSuchDevice);
        }
        let mac = info.mac.ok_or(AxError::NoSuchDevice)?;
        let mut sll_addr = [0; 8];
        sll_addr[..mac.0.len()].copy_from_slice(&mac.0);
        Ok(Self {
            sll_family: AF_PACKET as u16,
            sll_protocol: protocol,
            sll_ifindex: info.id.to_linux_ifindex(),
            sll_hatype: ARPHRD_ETHER,
            sll_pkttype: PACKET_HOST,
            sll_halen: mac.0.len() as u8,
            sll_addr,
        })
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

struct PacketSocketState {
    bound: SockAddrLl,
    pending: Option<(Vec<u8>, SockAddrLl)>,
}

pub struct PacketSocket {
    state: PiMutex<PacketSocketState>,
    non_blocking: AtomicBool,
    poll_rx: PollSet,
}

impl PacketSocket {
    pub fn new(protocol: u16) -> AxResult<Self> {
        if !in_root_net_ns() {
            return Err(AxError::PermissionDenied);
        }
        let info = first_visible_ethernet()?;
        Ok(Self {
            state: PiMutex::new(PacketSocketState {
                bound: SockAddrLl::from_interface(&info, protocol)?,
                pending: None,
            }),
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        })
    }

    pub fn bind_ll(&self, addr: SockAddrLl) -> AxResult<()> {
        if !in_root_net_ns() {
            return Err(AxError::NoSuchDevice);
        }
        let info = if addr.sll_ifindex == 0 {
            first_visible_ethernet()?
        } else {
            let id =
                InterfaceId::from_linux_ifindex(addr.sll_ifindex).ok_or(AxError::InvalidInput)?;
            visible_interface_by_id(id)?
        };
        // from_interface checks kind, no need to check again
        let mut state = self.state.lock();
        state.bound = SockAddrLl::from_interface(&info, addr.sll_protocol)?;
        Ok(())
    }

    pub fn local_addr(&self) -> SockAddrLl {
        self.state.lock().bound
    }

    pub fn send_packet(&self, src: &mut IoSrc) -> AxResult<usize> {
        if !in_root_net_ns() {
            return Err(AxError::NoSuchDevice);
        }
        let len = src.remaining();
        if len == 0 {
            return Ok(0);
        }
        let mut data = vec![0; len];
        let read = src.read(&mut data)?;
        data.truncate(read);

        let bound = self.state.lock().bound;
        if let Some(reply) = build_arp_reply(&data, bound) {
            {
                self.state.lock().pending = Some(reply);
            }
            // Pending packet is stored before waking readers.
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
        Ok(read)
    }

    pub fn recv_packet(&self, dst: &mut IoDst) -> AxResult<(usize, SockAddrLl)> {
        let task = current_user_task();
        block_on_user(
            &task,
            poll_io_for(&task, self, IoEvents::IN, self.nonblocking(), || {
                let (data, from) = {
                    let mut state = self.state.lock();
                    state.pending.take().ok_or(AxError::WouldBlock)?
                };
                let written = dst.write(&data)?;
                Ok((written, from))
            }),
        )
    }

    pub fn from_fd(fd: c_int) -> AxResult<Arc<Self>> {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| AxError::NotASocket)
    }
}

fn build_arp_reply(request: &[u8], bound: SockAddrLl) -> Option<(Vec<u8>, SockAddrLl)> {
    let id = InterfaceId::from_linux_ifindex(bound.sll_ifindex)?;
    let info = visible_interface_by_id(id).ok()?;
    let mac = info.mac?;
    if request.len() < 28
        || u16::from_be_bytes([request[0], request[1]]) != ARPHRD_ETHER
        || u16::from_be_bytes([request[2], request[3]]) != ETH_P_IP
        || request[4] != mac.0.len() as u8
        || request[5] != 4
        || u16::from_be_bytes([request[6], request[7]]) != ARPOP_REQUEST
    {
        return None;
    }

    let request_sender_protocol: [u8; 4] = request[14..18].try_into().ok()?;
    let request_target_protocol: [u8; 4] = request[24..28].try_into().ok()?;
    if !is_modeled_peer_ipv4(&info, request_target_protocol) {
        return None;
    }

    let mut reply = request.to_vec();
    reply[6..8].copy_from_slice(&ARPOP_REPLY.to_be_bytes());
    reply[8..14].copy_from_slice(&SYNTHETIC_PEER_HWADDR);
    reply[14..18].copy_from_slice(&request_target_protocol);
    reply[18..24].copy_from_slice(&request[8..14]);
    reply[24..28].copy_from_slice(&request_sender_protocol);

    let mut from = SockAddrLl::from_interface(&info, ETH_P_ARP.to_be()).ok()?;
    from.sll_addr[..SYNTHETIC_PEER_HWADDR.len()].copy_from_slice(&SYNTHETIC_PEER_HWADDR);

    Some((reply, from))
}

fn is_modeled_peer_ipv4(info: &InterfaceInfo, ip: [u8; 4]) -> bool {
    info.ipv4
        .and_then(|config| config.gateway)
        .is_some_and(|gateway| gateway.octets() == ip)
}

fn read_user_bytes<const N: usize>(ptr: *const u8) -> AxResult<[u8; N]> {
    let mut buf = [core::mem::MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|b| unsafe { b.assume_init() }))
}

fn ifreq_interface(arg: usize) -> AxResult<InterfaceInfo> {
    let name = read_user_bytes::<IFREQ_NAME_LEN>(arg as *const u8)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let name = core::str::from_utf8(&name[..end]).map_err(|_| AxError::InvalidInput)?;
    ax_net::interface_by_name(name).ok_or(AxError::NoSuchDevice)
}

fn write_ifreq_data(arg: usize, data: &[u8]) -> AxResult<()> {
    Ok(vm_write_slice((arg + IFREQ_DATA_OFFSET) as *mut u8, data)?)
}

fn linux_flags(info: &InterfaceInfo) -> i16 {
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
        flags |= IFF_RUNNING;
    }
    if info.flags.contains(InterfaceFlags::MULTICAST) {
        flags |= IFF_MULTICAST;
    }
    flags
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
        if !in_root_net_ns() {
            return Err(AxError::NoSuchDevice);
        }
        let info = ifreq_interface(arg)?;

        match cmd {
            SIOCGIFINDEX => write_ifreq_data(arg, &info.id.to_linux_ifindex().to_ne_bytes())?,
            SIOCGIFFLAGS => write_ifreq_data(arg, &linux_flags(&info).to_ne_bytes())?,
            SIOCGIFHWADDR => {
                let mac = info.mac.ok_or(AxError::NoSuchDevice)?;
                let mut hwaddr = [0; 16];
                hwaddr[..2].copy_from_slice(&ARPHRD_ETHER.to_ne_bytes());
                hwaddr[2..2 + mac.0.len()].copy_from_slice(&mac.0);
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
            // Registration happens from socket poll task context.
            unsafe { self.poll_rx.register(context.waker(), IoEvents::IN) };
        }
    }
}
