use alloc::{
    borrow::{Cow, ToOwned},
    format,
    sync::Arc,
};
use core::{
    ffi::c_int,
    mem::offset_of,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_net::{
    InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind, RecvOptions, SendOptions,
    Socket as SocketInner, SocketOps,
    options::{Configurable, GetSocketOption, SetSocketOption},
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    ioctl::{
        FIONREAD, SIOCGIFADDR, SIOCGIFBRDADDR, SIOCGIFCONF, SIOCGIFDSTADDR, SIOCGIFFLAGS,
        SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMAP, SIOCGIFMETRIC, SIOCGIFMTU, SIOCGIFNETMASK,
        SIOCGIFTXQLEN,
    },
    net::{AF_INET, ifreq},
};
use starry_vm::{VmMutPtr, vm_read_slice, vm_write_slice};

use super::{FileLike, Kstat};
use crate::{
    file::{IoDst, IoSrc, get_file_like},
    syscall::in_root_net_ns,
};

pub(super) const ARPHRD_ETHER: u16 = 1;
pub(super) const ARPHRD_LOOPBACK: u16 = 772;
const IFF_UP: i16 = 0x0001;
const IFF_BROADCAST: i16 = 0x0002;
const IFF_LOOPBACK: i16 = 0x0008;
const IFF_RUNNING: i16 = 0x0040;
const IFF_MULTICAST: i16 = 0x1000;
const IFREQ_NAME_LEN: usize = 16;
const IFREQ_DATA_OFFSET: usize = 16;
const IFREQ_COMPAT_LEN: usize = 40;
const IFCONF_LEN_OFFSET: usize = 0;
const IFCONF_BUF_OFFSET: usize = 8;

pub struct Socket {
    inner: SocketInner,
    ip_domain: u32,
    async_mode: AtomicBool,
    owner: AtomicI32,
}

impl Socket {
    pub fn new(inner: SocketInner, ip_domain: u32) -> Self {
        Self {
            inner,
            ip_domain,
            async_mode: AtomicBool::new(false),
            owner: AtomicI32::new(0),
        }
    }

    pub fn ip_domain(&self) -> u32 {
        self.ip_domain
    }
}

pub(super) fn visible_interfaces() -> impl Iterator<Item = InterfaceInfo> {
    ax_net::interfaces()
        .into_iter()
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
}

pub(super) fn visible_interface_by_id(id: InterfaceId) -> AxResult<InterfaceInfo> {
    ax_net::interface_by_id(id)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(AxError::NoSuchDevice)
}

pub(super) fn first_visible_ethernet() -> AxResult<InterfaceInfo> {
    visible_interfaces()
        .find(|info| info.kind == InterfaceKind::Ethernet)
        .ok_or(AxError::NoSuchDevice)
}

fn read_user_bytes<const N: usize>(ptr: *const u8) -> AxResult<[u8; N]> {
    let mut buf = [core::mem::MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn read_ifreq_name(arg: usize) -> AxResult<alloc::string::String> {
    let name = read_user_bytes::<IFREQ_NAME_LEN>(arg as *const u8)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..end])
        .map(str::to_owned)
        .map_err(|_| AxError::InvalidInput)
}

fn read_ifreq_interface(arg: usize) -> AxResult<InterfaceInfo> {
    let name = read_ifreq_name(arg)?;
    ax_net::interface_by_name(&name)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(AxError::NoSuchDevice)
}

fn write_ifreq_data(arg: usize, data: &[u8]) -> AxResult<()> {
    Ok(vm_write_slice((arg + IFREQ_DATA_OFFSET) as *mut u8, data)?)
}

fn sockaddr_in_bytes(ip: [u8; 4]) -> [u8; 16] {
    let mut addr = [0; 16];
    addr[..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    addr[4..8].copy_from_slice(&ip);
    addr
}

fn write_ifreq_sockaddr(arg: usize, ip: [u8; 4]) -> AxResult<()> {
    write_ifreq_data(arg, &sockaddr_in_bytes(ip))
}

fn write_ifreq_hwaddr(arg: usize, hw_type: u16, hwaddr: &[u8]) -> AxResult<()> {
    let mut addr = [0; 16];
    addr[..2].copy_from_slice(&hw_type.to_ne_bytes());
    addr[2..2 + hwaddr.len()].copy_from_slice(hwaddr);
    write_ifreq_data(arg, &addr)
}

fn write_ifconf_entry(buf: usize, offset: usize, name: &str, ip: [u8; 4]) -> AxResult<()> {
    let mut ifreq = [0; IFREQ_COMPAT_LEN];
    let name = name.as_bytes();
    let name_len = name.len().min(IFREQ_NAME_LEN - 1);
    ifreq[..name_len].copy_from_slice(&name[..name_len]);
    ifreq[IFREQ_DATA_OFFSET..IFREQ_DATA_OFFSET + 16].copy_from_slice(&sockaddr_in_bytes(ip));
    Ok(vm_write_slice((buf + offset) as *mut u8, &ifreq)?)
}

fn interface_ipv4(info: &InterfaceInfo) -> AxResult<ax_net::Ipv4InterfaceConfig> {
    info.ipv4.ok_or(AxError::NoSuchDeviceOrAddress)
}

fn ipv4_netmask(prefix_len: u8) -> [u8; 4] {
    if prefix_len == 0 {
        return [0; 4];
    }
    (!0u32 << (32 - prefix_len)).to_be_bytes()
}

fn ipv4_broadcast(config: ax_net::Ipv4InterfaceConfig) -> [u8; 4] {
    let ip = u32::from_be_bytes(config.address.address().octets());
    let mask = u32::from_be_bytes(ipv4_netmask(config.address.prefix_len()));
    (ip | !mask).to_be_bytes()
}

fn linux_flags(info: &InterfaceInfo) -> i16 {
    let mut flags = 0;
    if info.flags.contains(InterfaceFlags::UP) {
        flags |= IFF_UP;
    }
    if info.flags.contains(InterfaceFlags::RUNNING) {
        flags |= IFF_RUNNING;
    }
    if info.flags.contains(InterfaceFlags::LOOPBACK) {
        flags |= IFF_LOOPBACK;
    }
    if info.flags.contains(InterfaceFlags::BROADCAST) {
        flags |= IFF_BROADCAST;
    }
    if info.flags.contains(InterfaceFlags::MULTICAST) {
        flags |= IFF_MULTICAST;
    }
    flags
}

fn write_ifconf(arg: usize) -> AxResult<()> {
    let mut len = read_user_bytes::<4>((arg + IFCONF_LEN_OFFSET) as *const u8)?;
    let ifc_len = i32::from_ne_bytes(len);
    let buf = usize::from_ne_bytes(read_user_bytes::<{ core::mem::size_of::<usize>() }>(
        (arg + IFCONF_BUF_OFFSET) as *const u8,
    )?);
    let interfaces: alloc::vec::Vec<_> = visible_interfaces()
        .filter_map(|info| {
            info.ipv4
                .map(|ipv4| (info.name, ipv4.address.address().octets()))
        })
        .collect();

    if buf != 0 {
        let mut written = 0;
        for (name, ip) in interfaces {
            if ifc_len < (written + IFREQ_COMPAT_LEN) as i32 {
                break;
            }
            write_ifconf_entry(buf, written, &name, ip)?;
            written += IFREQ_COMPAT_LEN;
        }
        len = (written as i32).to_ne_bytes();
    } else {
        len = ((interfaces.len() * IFREQ_COMPAT_LEN) as i32).to_ne_bytes();
    }
    vm_write_slice((arg + IFCONF_LEN_OFFSET) as *mut u8, &len)?;
    Ok(())
}

impl Deref for Socket {
    type Target = SocketInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FileLike for Socket {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.recv(dst, RecvOptions::default())
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        self.send(src, SendOptions::default())
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat {
            mode: S_IFSOCK | 0o777u32,
            blksize: 4096,
            ..Default::default()
        })
    }

    fn nonblocking(&self) -> bool {
        let mut result = false;
        self.get_option(GetSocketOption::NonBlocking(&mut result))
            .unwrap();
        result
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult<()> {
        self.inner
            .set_option(SetSocketOption::NonBlocking(&nonblocking))
    }

    fn async_mode(&self) -> bool {
        self.async_mode.load(Ordering::Acquire)
    }

    fn supports_async_mode(&self) -> bool {
        true
    }

    fn set_async_mode(&self, async_mode: bool) -> AxResult {
        self.async_mode.store(async_mode, Ordering::Release);
        Ok(())
    }

    fn owner(&self) -> AxResult<i32> {
        Ok(self.owner.load(Ordering::Acquire))
    }

    fn set_owner(&self, owner: i32) -> AxResult {
        self.owner.store(owner, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        format!("socket:[{}]", self as *const _ as usize).into()
    }

    fn open_flags(&self) -> u32 {
        O_RDWR
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            FIONREAD => {
                let available = self.inner.recv_available()?.min(c_int::MAX as usize) as c_int;
                (arg as *mut c_int).vm_write(available)?;
            }
            SIOCGIFCONF => write_ifconf(arg)?,
            SIOCGIFFLAGS => {
                let info = read_ifreq_interface(arg)?;
                write_ifreq_data(arg, &linux_flags(&info).to_ne_bytes())?;
            }
            SIOCGIFADDR => {
                let info = read_ifreq_interface(arg)?;
                write_ifreq_sockaddr(arg, interface_ipv4(&info)?.address.address().octets())?;
            }
            SIOCGIFDSTADDR => {
                let info = read_ifreq_interface(arg)?;
                let addr = if info.kind == InterfaceKind::Loopback {
                    interface_ipv4(&info)?.address.address().octets()
                } else {
                    [0, 0, 0, 0]
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFBRDADDR => {
                let info = read_ifreq_interface(arg)?;
                let addr = if info.kind == InterfaceKind::Loopback {
                    interface_ipv4(&info)?.address.address().octets()
                } else {
                    ipv4_broadcast(interface_ipv4(&info)?)
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFNETMASK => {
                let info = read_ifreq_interface(arg)?;
                write_ifreq_sockaddr(
                    arg,
                    ipv4_netmask(interface_ipv4(&info)?.address.prefix_len()),
                )?;
            }
            SIOCGIFHWADDR => {
                let info = read_ifreq_interface(arg)?;
                match info.kind {
                    InterfaceKind::Ethernet => {
                        let mac = info.mac.ok_or(AxError::NoSuchDevice)?;
                        write_ifreq_hwaddr(arg, ARPHRD_ETHER, &mac.0)?
                    }
                    InterfaceKind::Loopback => write_ifreq_hwaddr(arg, ARPHRD_LOOPBACK, &[])?,
                }
            }
            SIOCGIFMTU => {
                let mtu = read_ifreq_interface(arg)?.mtu as i32;
                write_ifreq_data(arg, &mtu.to_ne_bytes())?;
            }
            SIOCGIFMETRIC => {
                read_ifreq_interface(arg)?;
                write_ifreq_data(arg, &0i32.to_ne_bytes())?;
            }
            SIOCGIFMAP => {
                read_ifreq_interface(arg)?;
                write_ifreq_data(arg, &[0; 24])?;
            }
            SIOCGIFTXQLEN => {
                read_ifreq_interface(arg)?;
                let qlen_ptr = (arg + offset_of!(ifreq, ifr_ifru)) as *mut i32;
                qlen_ptr.vm_write(1000)?;
            }
            SIOCGIFINDEX => {
                let idx = read_ifreq_interface(arg)?.id.get() as i32;
                write_ifreq_data(arg, &idx.to_ne_bytes())?;
            }
            _ => {
                if super::wext::is_wext_ioctl(cmd) {
                    return super::wext::handle(cmd, arg);
                }
                return Err(AxError::NotATty);
            }
        }
        Ok(0)
    }

    fn from_fd(fd: c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| AxError::NotASocket)
    }
}

impl Pollable for Socket {
    fn poll(&self) -> IoEvents {
        self.inner.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner.register(context, events);
    }
}
