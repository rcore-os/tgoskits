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
use ax_task::current;
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::{
    general::{O_RDWR, S_IFSOCK},
    ioctl::{
        FIONREAD, SIOCADDRT, SIOCDELRT, SIOCGIFADDR, SIOCGIFBRDADDR, SIOCGIFCONF, SIOCGIFDSTADDR,
        SIOCGIFFLAGS, SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMAP, SIOCGIFMETRIC, SIOCGIFMTU,
        SIOCGIFNAME, SIOCGIFNETMASK, SIOCGIFTXQLEN, SIOCSIFADDR, SIOCSIFFLAGS, SIOCSIFMTU,
        SIOCSIFNETMASK,
    },
    net::{AF_INET, ifreq},
};
use starry_vm::{VmMutPtr, vm_read_slice, vm_write_slice};

use super::{FileLike, Kstat};
use crate::{
    file::{IoDst, IoSrc, get_file_like},
    syscall::in_root_net_ns,
    task::AsThread,
};

pub(super) const ARPHRD_ETHER: u16 = 1;
pub(super) const ARPHRD_LOOPBACK: u16 = 772;
/// `ARPHRD_NONE`: no link-layer header, as reported by a layer-3 TUN device.
pub(super) const ARPHRD_NONE: u16 = 0xFFFE;
const IFF_UP: i16 = 0x0001;
const IFF_BROADCAST: i16 = 0x0002;
const IFF_LOOPBACK: i16 = 0x0008;
const IFF_POINTOPOINT: i16 = 0x0010;
const IFF_NOARP: i16 = 0x0080;
const IFF_RUNNING: i16 = 0x0040;
const IFF_MULTICAST: i16 = 0x1000;
const IFREQ_NAME_LEN: usize = 16;
const IFREQ_DATA_OFFSET: usize = 16;
const IFREQ_COMPAT_LEN: usize = 40;
// ethtool ioctl; not exported by linux-raw-sys. The value is arch-independent.
const SIOCETHTOOL: u32 = 0x8946;
const IFCONF_LEN_OFFSET: usize = 0;
const IFCONF_BUF_OFFSET: usize = 8;

/// Prefix used by `SIOCSIFADDR` before a netmask is supplied. A `/24` matches
/// the common private-subnet convention and can be refined by `SIOCSIFNETMASK`.
const DEFAULT_IPV4_PREFIX: u8 = 24;

// Field offsets inside `struct rtentry` on 64-bit Linux. Each embedded
// `sockaddr` places `sin_addr` four bytes past its `sin_family`, so the address
// offset is the sockaddr offset plus four.
const RT_DST_ADDR_OFFSET: usize = 8; // rt_dst sockaddr at 8
const RT_GATEWAY_ADDR_OFFSET: usize = 24; // rt_gateway sockaddr at 24
const RT_GENMASK_ADDR_OFFSET: usize = 40; // rt_genmask sockaddr at 40
const RT_DEV_PTR_OFFSET: usize = 88; // char *rt_dev

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

fn read_ifreq_flags(arg: usize) -> AxResult<i16> {
    Ok(i16::from_ne_bytes(read_user_bytes::<2>(
        (arg + IFREQ_DATA_OFFSET) as *const u8,
    )?))
}

fn write_ifreq_name(arg: usize, name: &str) -> AxResult<()> {
    let mut buf = [0u8; IFREQ_NAME_LEN];
    let bytes = name.as_bytes();
    let len = bytes.len().min(IFREQ_NAME_LEN - 1);
    buf[..len].copy_from_slice(&bytes[..len]);
    Ok(vm_write_slice(arg as *mut u8, &buf)?)
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

/// Reads a `sockaddr_in` embedded in an `ifreq`/`rtentry` and returns its
/// IPv4 address, rejecting a non-`AF_INET` family the way Linux does.
///
/// `offset` is the byte offset of the `sockaddr` within the ioctl argument
/// struct; `sin_family` is at `offset` and `sin_addr` at `offset + 4`.
fn read_sockaddr_ipv4(arg: usize, offset: usize) -> AxResult<core::net::Ipv4Addr> {
    let family = u16::from_ne_bytes(read_user_bytes::<2>((arg + offset) as *const u8)?);
    if family != AF_INET as u16 {
        return Err(AxError::InvalidInput);
    }
    let octets = read_user_bytes::<4>((arg + offset + 4) as *const u8)?;
    Ok(core::net::Ipv4Addr::from(octets))
}

/// Converts a contiguous IPv4 netmask into a CIDR prefix length, rejecting a
/// non-contiguous mask.
fn prefix_from_netmask(mask: core::net::Ipv4Addr) -> AxResult<u8> {
    let bits = u32::from_be_bytes(mask.octets());
    let ones = bits.leading_ones();
    // A valid mask is a run of 1s followed by 0s: reconstructing it from the
    // count must reproduce the original.
    let rebuilt = if ones == 0 { 0 } else { !0u32 << (32 - ones) };
    if rebuilt == bits {
        Ok(ones as u8)
    } else {
        Err(AxError::InvalidInput)
    }
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
    if info.flags.contains(InterfaceFlags::POINTOPOINT) {
        flags |= IFF_POINTOPOINT;
    }
    if info.flags.contains(InterfaceFlags::NOARP) {
        flags |= IFF_NOARP;
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

/// Reads the NUL-terminated interface name pointed to by `rt_dev`.
fn read_route_device(arg: usize) -> AxResult<Option<alloc::string::String>> {
    let ptr = usize::from_ne_bytes(read_user_bytes::<{ core::mem::size_of::<usize>() }>(
        (arg + RT_DEV_PTR_OFFSET) as *const u8,
    )?);
    if ptr == 0 {
        return Ok(None);
    }
    // The device name is at most IFNAMSIZ bytes; read the fixed window and stop
    // at the first NUL.
    let bytes = read_user_bytes::<IFREQ_NAME_LEN>(ptr as *const u8)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end])
        .map(|s| Some(s.to_owned()))
        .map_err(|_| AxError::InvalidInput)
}

/// Handles `SIOCADDRT`/`SIOCDELRT` by parsing a `struct rtentry` and installing
/// or removing an IPv4 route on the interface named by `rt_dev`.
fn write_route(arg: usize, add: bool) -> AxResult<()> {
    let dst = read_sockaddr_ipv4(arg, RT_DST_ADDR_OFFSET)?;
    // A non-AF_INET gateway sockaddr (family AF_UNSPEC / 0) means a directly
    // connected route; EFAULT from a bad user pointer must still propagate.
    let gateway = match read_sockaddr_ipv4(arg, RT_GATEWAY_ADDR_OFFSET) {
        Ok(ip) => Some(ip),
        Err(AxError::InvalidInput) => None,
        Err(e) => return Err(e),
    };
    // Netmask family is often left unset (AF_UNSPEC = default route /0); EFAULT
    // propagates, other parse failures default to a /0 prefix.
    let prefix = match read_sockaddr_ipv4(arg, RT_GENMASK_ADDR_OFFSET) {
        Ok(mask) => prefix_from_netmask(mask)?,
        Err(AxError::InvalidInput) => 0,
        Err(e) => return Err(e),
    };

    let device = read_route_device(arg)?.ok_or(AxError::NoSuchDevice)?;
    let info = ax_net::interface_by_name(&device)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(AxError::NoSuchDevice)?;

    if add {
        ax_net::add_route(info.id, dst, prefix, gateway)?;
    } else {
        ax_net::del_route(info.id, dst, prefix, gateway)?;
    }
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
        // Mutating network ioctls change global interface/route state and require
        // CAP_NET_ADMIN, matching Linux (`devinet_ioctl` gates SIOCSIF*
        // address/netmask/flags/mtu and `ip_rt_ioctl` gates SIOCADDRT/SIOCDELRT on
        // CAP_NET_ADMIN). Query ioctls (SIOCGIF*, FIONREAD, ...) stay available to
        // unprivileged callers.
        if matches!(
            cmd,
            SIOCSIFADDR | SIOCSIFNETMASK | SIOCSIFFLAGS | SIOCSIFMTU | SIOCADDRT | SIOCDELRT
        ) && !current().as_thread().cred().has_cap_net_admin()
        {
            return Err(AxError::OperationNotPermitted);
        }
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
                    InterfaceKind::Ethernet | InterfaceKind::Tap => {
                        let mac = info.mac.ok_or(AxError::NoSuchDevice)?;
                        write_ifreq_hwaddr(arg, ARPHRD_ETHER, &mac.0)?
                    }
                    InterfaceKind::Loopback => write_ifreq_hwaddr(arg, ARPHRD_LOOPBACK, &[])?,
                    // A layer-3 TUN has no link-layer address; Linux reports
                    // ARPHRD_NONE with an all-zero hardware address.
                    InterfaceKind::Tun => write_ifreq_hwaddr(arg, ARPHRD_NONE, &[])?,
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
            SIOCGIFNAME => {
                // Reverse of SIOCGIFINDEX: resolve the interface by the caller-supplied
                // ifr_ifindex and write its name back into ifr_name. dnsmasq's indextoname()
                // drives this during interface enumeration.
                let idx = i32::from_ne_bytes(read_user_bytes::<4>(
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                let id = InterfaceId::from_linux_ifindex(idx).ok_or(AxError::NoSuchDevice)?;
                let info = visible_interface_by_id(id)?;
                write_ifreq_name(arg, &info.name)?;
            }
            SIOCSIFADDR => {
                // Assign an IPv4 address. Linux derives the prefix from the
                // interface's current netmask, defaulting to the class mask;
                // StarryOS keeps a single address per interface, so a /24 is a
                // sensible default that `SIOCSIFNETMASK` cannot yet refine.
                let info = read_ifreq_interface(arg)?;
                let ip = read_sockaddr_ipv4(arg, IFREQ_DATA_OFFSET)?;
                ax_net::set_interface_ipv4(info.id, ip, DEFAULT_IPV4_PREFIX)?;
            }
            SIOCSIFNETMASK => {
                // Re-apply the address under the requested prefix. Userspace
                // tools issue SIOCSIFADDR then SIOCSIFNETMASK; the second call
                // adjusts the connected route to the intended CIDR.
                let info = read_ifreq_interface(arg)?;
                let mask = read_sockaddr_ipv4(arg, IFREQ_DATA_OFFSET)?;
                let prefix = prefix_from_netmask(mask)?;
                let address = interface_ipv4(&info)?.address;
                let ip = core::net::Ipv4Addr::from(address.address().octets());
                ax_net::remove_interface_ipv4(info.id, ip, address.prefix_len())?;
                ax_net::set_interface_ipv4(info.id, ip, prefix)?;
            }
            SIOCSIFFLAGS => {
                let info = read_ifreq_interface(arg)?;
                let flags = read_ifreq_flags(arg)?;
                ax_net::set_interface_flags(info.id, flags & IFF_UP != 0)?;
            }
            SIOCSIFMTU => {
                // The single IP-medium router MTU is fixed; accept a request to
                // set it to the current value and reject a different one so a
                // caller cannot silently believe a larger MTU took effect.
                let info = read_ifreq_interface(arg)?;
                let requested = i32::from_ne_bytes(read_user_bytes::<4>(
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                if requested != info.mtu as i32 {
                    return Err(AxError::InvalidInput);
                }
            }
            SIOCADDRT => write_route(arg, true)?,
            SIOCDELRT => write_route(arg, false)?,
            // Link speed/duplex query. No PHY is emulated, so report "not supported" the way a
            // virtual NIC (loopback, tun/tap) does. Tools like psutil's net_if_stats() treat
            // EOPNOTSUPP as "no ethtool" and degrade gracefully; any other errno makes them abort
            // the whole interface-status probe. Resolve the interface first so an unknown name
            // yields ENODEV, then fault on a bad ifr_data pointer, keeping Linux's error priority
            // (ENODEV, then EFAULT, then EOPNOTSUPP) and parity with the sibling SIOC*IF* arms.
            SIOCETHTOOL => {
                read_ifreq_interface(arg)?;
                let data_ptr = usize::from_ne_bytes(read_user_bytes::<8>(
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                read_user_bytes::<4>(data_ptr as *const u8)?;
                return Err(AxError::OperationNotSupported);
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
