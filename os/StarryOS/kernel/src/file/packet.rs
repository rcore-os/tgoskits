use alloc::{borrow::Cow, format, sync::Arc, vec::Vec};
use core::{
    ffi::c_int,
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_net::{InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind};
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io};
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
};

const PACKET_HOST: u8 = 0;
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
    state: Mutex<PacketSocketState>,
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
            state: Mutex::new(PacketSocketState {
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

    pub fn send_packet(&self, _src: &mut IoSrc) -> AxResult<usize> {
        if !in_root_net_ns() {
            return Err(AxError::NoSuchDevice);
        }
        // TODO: Real packet transmission through ax-net
        // Not yet implemented - return error instead of silently discarding
        Err(AxError::OperationNotSupported)
    }

    pub fn recv_packet(&self, dst: &mut IoDst) -> AxResult<(usize, SockAddrLl)> {
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let (data, from) = {
                let mut state = self.state.lock();
                state.pending.take().ok_or(AxError::WouldBlock)?
            };
            let written = dst.write(&data)?;
            Ok((written, from))
        }))
    }

    pub fn from_fd(fd: c_int) -> AxResult<Arc<Self>> {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| AxError::NotASocket)
    }
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
            self.poll_rx.register(context.waker());
        }
    }
}
