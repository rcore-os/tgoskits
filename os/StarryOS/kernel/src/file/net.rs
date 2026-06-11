use alloc::{borrow::Cow, format, sync::Arc};
use core::{
    ffi::c_int,
    mem::offset_of,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_net::{
    RecvOptions, SendOptions, Socket as SocketInner, SocketOps,
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

/// Real eth0 MAC address. Uses the QEMU default; TODO: query
/// `EthernetDriver::mac_address()` from ax_net at init time once the API is
/// exposed, then replace this with a `static` or `LazyLock`.
pub const ETH0_REAL_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

pub(super) const ETH0_IFINDEX: i32 = 2;
pub(super) const LO_IFINDEX: i32 = 1;
const ETH0_NAME: &[u8] = b"eth0";
const LO_NAME: &[u8] = b"lo";
const ARPHRD_ETHER: u16 = 1;
const ARPHRD_LOOPBACK: u16 = 772;
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
const ETH0_MTU: i32 = 1500;
const LO_MTU: i32 = 65536;

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

#[derive(Clone, Copy)]
enum NetInterface {
    Eth0,
    Loopback,
}

fn eth0_ipv4_config() -> AxResult<ax_net::Ipv4InterfaceConfig> {
    ax_net::eth0_ipv4_config().ok_or(AxError::NoSuchDevice)
}

fn eth0_ipv4_addr() -> AxResult<[u8; 4]> {
    Ok(eth0_ipv4_config()?.address.address().octets())
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

fn read_user_bytes<const N: usize>(ptr: *const u8) -> AxResult<[u8; N]> {
    let mut buf = [core::mem::MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn read_ifreq_interface(arg: usize) -> AxResult<NetInterface> {
    let name = read_user_bytes::<IFREQ_NAME_LEN>(arg as *const u8)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    match &name[..end] {
        ETH0_NAME => {
            if !in_root_net_ns() {
                return Err(AxError::NoSuchDevice);
            }
            Ok(NetInterface::Eth0)
        }
        LO_NAME => Ok(NetInterface::Loopback),
        _ => Err(AxError::NoSuchDevice),
    }
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

fn write_ifconf_entry(buf: usize, offset: usize, name: &[u8], ip: [u8; 4]) -> AxResult<()> {
    let mut ifreq = [0; IFREQ_COMPAT_LEN];
    ifreq[..name.len()].copy_from_slice(name);
    ifreq[IFREQ_DATA_OFFSET..IFREQ_DATA_OFFSET + 16].copy_from_slice(&sockaddr_in_bytes(ip));
    Ok(vm_write_slice((buf + offset) as *mut u8, &ifreq)?)
}

fn write_eth0_ifconf(arg: usize) -> AxResult<()> {
    let mut len = read_user_bytes::<4>((arg + IFCONF_LEN_OFFSET) as *const u8)?;
    let ifc_len = i32::from_ne_bytes(len);
    let buf = usize::from_ne_bytes(read_user_bytes::<{ core::mem::size_of::<usize>() }>(
        (arg + IFCONF_BUF_OFFSET) as *const u8,
    )?);

    if buf != 0 {
        let mut written = 0;
        if in_root_net_ns() && ifc_len >= IFREQ_COMPAT_LEN as i32 {
            write_ifconf_entry(buf, written, ETH0_NAME, eth0_ipv4_addr()?)?;
            written += IFREQ_COMPAT_LEN;
        }
        if ifc_len >= (written + IFREQ_COMPAT_LEN) as i32 {
            write_ifconf_entry(buf, written, LO_NAME, [127, 0, 0, 1])?;
            written += IFREQ_COMPAT_LEN;
        }
        len = (written as i32).to_ne_bytes();
    } else {
        // SIOCGIFCONF sizing call (ifc_buf == NULL): Linux's dev_ifconf returns
        // the number of bytes needed to hold all interfaces so the caller can
        // size its buffer. Returning 0 made OpenJDK's
        // NetworkInterface.enumIPv4Interfaces malloc a 0-byte buffer and find no
        // interfaces. Report space for eth0 + lo.
        len = (2 * IFREQ_COMPAT_LEN as i32).to_ne_bytes();
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
        // TODO(mivik): implement stat for sockets
        Ok(Kstat {
            mode: S_IFSOCK | 0o777u32, // rwxrwxrwx
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
            SIOCGIFCONF => write_eth0_ifconf(arg)?,
            SIOCGIFFLAGS => {
                let flags = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST,
                    NetInterface::Loopback => IFF_UP | IFF_LOOPBACK | IFF_RUNNING,
                };
                write_ifreq_data(arg, &flags.to_ne_bytes())?;
            }
            SIOCGIFADDR => {
                let addr = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => eth0_ipv4_addr()?,
                    NetInterface::Loopback => [127, 0, 0, 1],
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFDSTADDR => {
                let addr = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => [0, 0, 0, 0],
                    NetInterface::Loopback => [127, 0, 0, 1],
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFBRDADDR => {
                let addr = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => ipv4_broadcast(eth0_ipv4_config()?),
                    NetInterface::Loopback => [127, 0, 0, 1],
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFNETMASK => {
                let addr = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => ipv4_netmask(eth0_ipv4_config()?.address.prefix_len()),
                    NetInterface::Loopback => [255, 0, 0, 0],
                };
                write_ifreq_sockaddr(arg, addr)?;
            }
            SIOCGIFHWADDR => match read_ifreq_interface(arg)? {
                NetInterface::Eth0 => write_ifreq_hwaddr(arg, ARPHRD_ETHER, &ETH0_REAL_MAC)?,
                NetInterface::Loopback => write_ifreq_hwaddr(arg, ARPHRD_LOOPBACK, &[])?,
            },
            SIOCGIFMTU => {
                let mtu = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => ETH0_MTU,
                    NetInterface::Loopback => LO_MTU,
                };
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
                let idx = match read_ifreq_interface(arg)? {
                    NetInterface::Eth0 => ETH0_IFINDEX,
                    NetInterface::Loopback => LO_IFINDEX,
                };
                write_ifreq_data(arg, &idx.to_ne_bytes())?;
            }
            _ => return Err(AxError::NotATty),
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
