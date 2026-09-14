use alloc::{
    borrow::{Cow, ToOwned},
    format,
    sync::Arc,
    vec::Vec,
};
use core::{
    ffi::c_int,
    mem::offset_of,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

use ax_io::{Cursor, IoBuf, IoBufMut, Read, Write};
use ax_net::{
    ConnectStatus, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind, NetError, RecvFlags,
    RecvOptions, SendFlags, SendOptions, Socket as SocketInner, SocketAddrEx, SocketOps,
    SocketWaitPolicy,
    options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
    poll_socket_io,
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::{
    general::{CAP_NET_ADMIN, O_RDWR, S_IFSOCK},
    ioctl::{
        FIONREAD, SIOCGIFADDR, SIOCGIFBRDADDR, SIOCGIFCONF, SIOCGIFDSTADDR, SIOCGIFFLAGS,
        SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMAP, SIOCGIFMETRIC, SIOCGIFMTU, SIOCGIFNETMASK,
        SIOCGIFSLAVE, SIOCGIFTXQLEN, SIOCSIFFLAGS, SIOCADDRT, SIOCDELRT, SIOCSIFADDR, SIOCSIFMTU,
        SIOCSIFNETMASK,
    },
    net::{AF_INET, ifreq},
};

use super::{FileLike, Kstat};
use crate::{
    Errno, StarryError, StarryResult,
    file::{IoDst, IoSrc, get_file_like},
    mm::{VmMutPtr, vm_read_slice, vm_write_slice},
    task::{
        UserTaskRef, current_pid_view, current_user_task,
        future::{UserWaitOutcome, block_on_user_timeout},
    },
};

pub(super) const ARPHRD_ETHER: u16 = 1;
pub(super) const ARPHRD_LOOPBACK: u16 = 772;
const ARPHRD_NONE: u16 = 0xfffe;
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
// Map an interface index to its name (Linux net/core/dev_ioctl.c dev_ifname).
// Arch-independent; the inverse of SIOCGIFINDEX.
const SIOCGIFNAME: u32 = 0x8910;
const IFCONF_LEN_OFFSET: usize = 0;
const IFCONF_BUF_OFFSET: usize = 8;
/// `struct rtentry` layout on the supported 64-bit targets.
const RTENTRY_LEN: usize = 120;
const RT_DST: usize = 8;
const RT_GATEWAY: usize = 24;
const RT_GENMASK: usize = 40;
const RT_FLAGS: usize = 56;
const RT_DEV: usize = 88;
const RTF_GATEWAY: u16 = 0x0002;
const RTF_HOST: u16 = 0x0004;
const SOCKET_RECEIVE_STAGING_LIMIT: usize = 64 * 1024;

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

    /// Copies a task-owned source into kernel memory before entering ax-net.
    ///
    /// Some transports invoke `Read` callbacks while holding IRQ-safe spin
    /// locks. User-memory access may fault and therefore must finish before
    /// crossing that lock boundary.
    pub(crate) fn send_from_user<S>(
        &self,
        src: &mut S,
        mut options: SendOptions,
    ) -> StarryResult<usize>
    where
        S: Read + IoBuf + ?Sized,
    {
        let mut staging = allocate_socket_staging(src.remaining())?;
        src.read_exact(&mut staging)?;
        let mut staging = Cursor::new(staging.as_slice());
        let policy = self
            .inner
            .send_wait_policy(options.flags.contains(SendFlags::DONTWAIT))?;
        wait_socket_io(
            &current_user_task(),
            self,
            IoEvents::OUT,
            policy,
            StarryError::WouldBlock,
            || self.inner.try_send(&mut staging, &mut options),
        )
    }

    /// Receives into kernel memory and copies to the task only after ax-net
    /// releases its transport locks.
    ///
    /// A bounded staging buffer avoids allocating an attacker-controlled read
    /// length. Returning a short stream read is valid, while 64 KiB still
    /// covers the maximum IP datagram. Datagram `MSG_TRUNC` keeps the transport
    /// return length even when only the staged prefix is copied.
    pub(crate) fn recv_to_user<D>(
        &self,
        dst: &mut D,
        mut options: RecvOptions<'_>,
    ) -> StarryResult<usize>
    where
        D: Write + IoBufMut + ?Sized,
    {
        let capacity = dst.remaining_mut().min(SOCKET_RECEIVE_STAGING_LIMIT);
        let mut buffer = allocate_socket_staging(capacity)?;
        let mut staging = Cursor::new(buffer.as_mut_slice());
        let policy = self
            .inner
            .recv_wait_policy(options.flags.contains(RecvFlags::DONTWAIT))?;
        let received = wait_socket_io(
            &current_user_task(),
            self,
            IoEvents::IN,
            policy,
            StarryError::WouldBlock,
            || self.inner.try_recv(&mut staging, &mut options),
        )?;
        let copied = staging.position() as usize;
        dst.write_all(&buffer[..copied])?;
        Ok(received)
    }

    /// Starts and, when required, waits for one connection attempt.
    pub(crate) fn connect_user(
        &self,
        current: &UserTaskRef,
        remote_addr: SocketAddrEx,
    ) -> StarryResult<()> {
        let policy = self.inner.send_wait_policy(false)?;
        match self.inner.start_connect(remote_addr)? {
            ConnectStatus::Connected => Ok(()),
            ConnectStatus::InProgress if policy.nonblocking => Err(StarryError::InProgress),
            ConnectStatus::InProgress => wait_socket_io(
                current,
                self,
                IoEvents::OUT,
                policy,
                StarryError::TimedOut,
                || match self.inner.connect_status()? {
                    ConnectStatus::Connected => Ok(()),
                    ConnectStatus::InProgress => Err(NetError::WouldBlock),
                },
            ),
        }
    }

    /// Waits for and removes one accepted connection from a listener.
    pub(crate) fn accept_user(&self, current: &UserTaskRef) -> StarryResult<SocketInner> {
        let policy = self.inner.recv_wait_policy(false)?;
        wait_socket_io(
            current,
            self,
            IoEvents::IN,
            policy,
            StarryError::WouldBlock,
            || self.inner.try_accept(),
        )
    }

    pub fn ip_domain(&self) -> u32 {
        self.ip_domain
    }

    /// Captures the current process generation for Unix socket ownership.
    pub(crate) fn current_unix_credentials() -> UnixCredentials {
        let current = current_user_task();
        let thread = current.as_thread();
        let credentials = thread.cred();
        let process_identity = thread.proc_data.identity();
        let pid = current_pid_view()
            .visible_number(&process_identity)
            .expect("Unix socket owner is visible in its active PID namespace")
            .get();
        UnixCredentials::from_parts(pid, credentials.uid, credentials.gid)
            .with_identity(process_identity)
    }

    /// Projects a transport-owned process generation into the caller's active
    /// PID namespace while retaining numeric credentials for generic users.
    pub(crate) fn project_unix_credentials(credentials: &UnixCredentials) -> UnixCredentials {
        let pid = credentials.identity::<crate::task::PidIdentity>().map_or(
            credentials.pid,
            |identity| {
                current_pid_view()
                    .visible_number(identity)
                    .map_or(0, |number| number.get())
            },
        );
        UnixCredentials::from_parts(pid, credentials.uid, credentials.gid)
    }

    pub(crate) fn with_current_sender_credentials(mut options: SendOptions) -> SendOptions {
        options.sender_credentials = Some(Self::current_unix_credentials());
        options
    }
}

fn wait_socket_io<P, F, T>(
    current: &UserTaskRef,
    pollable: &P,
    events: IoEvents,
    policy: SocketWaitPolicy,
    timeout_error: StarryError,
    operation: F,
) -> StarryResult<T>
where
    P: Pollable + ?Sized,
    F: FnMut() -> ax_net::NetResult<T>,
{
    match block_on_user_timeout(
        current,
        policy.timeout,
        poll_socket_io(pollable, events, policy.nonblocking, operation),
    ) {
        UserWaitOutcome::Ready(result) => Ok(result?),
        UserWaitOutcome::Interrupted if policy.timeout.is_some() => {
            Err(StarryError::InterruptedNoRestart)
        }
        UserWaitOutcome::Interrupted => Err(StarryError::Interrupted),
        UserWaitOutcome::TimedOut => Err(timeout_error),
    }
}

fn allocate_socket_staging(len: usize) -> StarryResult<Vec<u8>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(len)
        .map_err(|_| StarryError::NoMemory)?;
    buffer.resize(len, 0);
    Ok(buffer)
}

/// Returns whether the calling task can observe the root network namespace.
///
/// This query remains at the file-object boundary because these methods are
/// also invoked outside syscall dispatch. A future network-namespace ownership
/// refactor should attach the namespace to each socket, as Linux does, instead
/// of extending a syscall task capability into the portable socket layer.
pub(super) fn in_root_net_ns() -> bool {
    let current = current_user_task();
    let namespace = current.as_thread().proc_data.namespace_snapshot();
    namespace.net_ns.lock().ns_id == 0
}

pub(super) fn visible_interfaces() -> impl Iterator<Item = InterfaceInfo> {
    ax_net::interfaces()
        .into_iter()
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
}

pub(super) fn visible_interface_by_id(id: InterfaceId) -> StarryResult<InterfaceInfo> {
    ax_net::interface_by_id(id)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(StarryError::NoSuchDevice)
}

pub(super) fn first_visible_ethernet() -> StarryResult<InterfaceInfo> {
    visible_interfaces()
        .find(|info| info.kind == InterfaceKind::Ethernet)
        .ok_or(StarryError::NoSuchDevice)
}

fn read_user_bytes<const N: usize>(
    current: &crate::task::UserTaskRef,
    ptr: *const u8,
) -> StarryResult<[u8; N]> {
    let mut buf = [core::mem::MaybeUninit::<u8>::uninit(); N];
    vm_read_slice(current, ptr, &mut buf)?;
    Ok(buf.map(|v| unsafe { v.assume_init() }))
}

fn read_ifreq_name(
    current: &crate::task::UserTaskRef,
    arg: usize,
) -> StarryResult<alloc::string::String> {
    let name = read_user_bytes::<IFREQ_NAME_LEN>(current, arg as *const u8)?;
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    core::str::from_utf8(&name[..end])
        .map(str::to_owned)
        .map_err(|_| StarryError::InvalidInput)
}

fn read_ifreq_interface(
    current: &crate::task::UserTaskRef,
    arg: usize,
) -> StarryResult<InterfaceInfo> {
    let name = read_ifreq_name(current, arg)?;
    ax_net::interface_by_name(&name)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(StarryError::NoSuchDevice)
}

fn write_ifreq_data(
    current: &crate::task::UserTaskRef,
    arg: usize,
    data: &[u8],
) -> StarryResult<()> {
    Ok(vm_write_slice(
        current,
        (arg + IFREQ_DATA_OFFSET) as *mut u8,
        data,
    )?)
}

fn read_ifreq_flags(current: &crate::task::UserTaskRef, arg: usize) -> StarryResult<i16> {
    Ok(i16::from_ne_bytes(read_user_bytes::<2>(
        current,
        (arg + IFREQ_DATA_OFFSET) as *const u8,
    )?))
}

// Writes an interface name into `ifr_name` (offset 0), NUL-padded to IFNAMSIZ.
fn write_ifreq_name(
    current: &crate::task::UserTaskRef,
    arg: usize,
    name: &str,
) -> StarryResult<()> {
    let mut buf = [0u8; IFREQ_NAME_LEN];
    let bytes = name.as_bytes();
    let n = bytes.len().min(IFREQ_NAME_LEN - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    Ok(vm_write_slice(current, arg as *mut u8, &buf)?)
}

/// Device-level socket ioctls (`SIOCGIF*`), shared across every socket family.
///
/// Linux routes these through `sock_ioctl` -> `dev_ioctl` regardless of the
/// socket's address family (net/socket.c), so `AF_UNIX`/`AF_NETLINK` sockets
/// answer them too - `if_indextoname(3)` in musl issues `SIOCGIFNAME` on an
/// `AF_UNIX` socket, which must resolve rather than return `ENOTTY`. Returns
/// `Some(result)` when `cmd` is a device ioctl this layer owns, `None` otherwise
/// so the caller can try family-specific commands or fall back to `ENOTTY`.
pub(super) fn device_ioctl(
    current: &crate::task::UserTaskRef,
    cmd: u32,
    arg: usize,
) -> Option<StarryResult<usize>> {
    let result = (|| -> StarryResult<usize> {
        match cmd {
            SIOCGIFCONF => write_ifconf(current, arg)?,
            SIOCGIFNAME => {
                // Map ifr_ifindex -> ifr_name (Linux dev_ifname); inverse of
                // SIOCGIFINDEX. The index arrives in the ifr_ifru union.
                let idx = i32::from_ne_bytes(read_user_bytes::<4>(
                    current,
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                let info = visible_interface_by_id(InterfaceId::new(idx as u32))?;
                write_ifreq_name(current, arg, &info.name)?;
            }
            SIOCGIFFLAGS => {
                let info = read_ifreq_interface(current, arg)?;
                write_ifreq_data(current, arg, &linux_flags(&info).to_ne_bytes())?;
            }
            SIOCSIFFLAGS => {
                let flags = read_ifreq_flags(current, arg)?;
                require_net_admin(current)?;
                let info = read_ifreq_interface(current, arg)?;
                if info.kind.is_tun_tap() {
                    ax_net::set_interface_up(info.id, flags & IFF_UP != 0)?;
                } else if flags != linux_flags(&info) {
                    return Err(StarryError::OperationNotSupported);
                }
            }
            SIOCSIFMTU => {
                let mtu = i32::from_ne_bytes(read_user_bytes::<4>(
                    current,
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                require_net_admin(current)?;
                let info = read_ifreq_interface(current, arg)?;
                let mtu = usize::try_from(mtu).map_err(|_| StarryError::InvalidInput)?;
                ax_net::set_interface_mtu(info.id, mtu)?;
            }
            SIOCSIFADDR => {
                let sockaddr = read_ifreq_sockaddr(current, arg)?;
                require_net_admin(current)?;
                let ip = sockaddr_in_address(&sockaddr)?;
                set_interface_address(&read_ifreq_interface(current, arg)?, ip)?;
            }
            SIOCSIFNETMASK => {
                let sockaddr = read_ifreq_sockaddr(current, arg)?;
                require_net_admin(current)?;
                let mask = sockaddr_in_address(&sockaddr)?;
                set_interface_netmask(&read_ifreq_interface(current, arg)?, mask)?;
            }
            SIOCADDRT | SIOCDELRT => write_route(current, cmd, arg)?,
            SIOCGIFADDR => {
                let info = read_ifreq_interface(current, arg)?;
                write_ifreq_sockaddr(
                    current,
                    arg,
                    interface_ipv4(&info)?.address.address().octets(),
                )?;
            }
            SIOCGIFDSTADDR => {
                let info = read_ifreq_interface(current, arg)?;
                let addr = if matches!(info.kind, InterfaceKind::Loopback | InterfaceKind::Tun) {
                    interface_ipv4(&info)?.address.address().octets()
                } else {
                    [0, 0, 0, 0]
                };
                write_ifreq_sockaddr(current, arg, addr)?;
            }
            SIOCGIFBRDADDR => {
                let info = read_ifreq_interface(current, arg)?;
                let ipv4 = interface_ipv4(&info)?;
                let addr = match info.kind {
                    InterfaceKind::Loopback => ipv4.address.address().octets(),
                    // A point-to-point address carries no broadcast.
                    InterfaceKind::Tun => [0; 4],
                    InterfaceKind::Ethernet | InterfaceKind::Tap => ipv4_broadcast(ipv4),
                };
                write_ifreq_sockaddr(current, arg, addr)?;
            }
            SIOCGIFNETMASK => {
                let info = read_ifreq_interface(current, arg)?;
                write_ifreq_sockaddr(
                    current,
                    arg,
                    ipv4_netmask(interface_ipv4(&info)?.address.prefix_len()),
                )?;
            }
            SIOCGIFHWADDR => {
                let info = read_ifreq_interface(current, arg)?;
                match info.kind {
                    InterfaceKind::Ethernet | InterfaceKind::Tap => {
                        let mac = info.mac.ok_or(StarryError::NoSuchDevice)?;
                        write_ifreq_hwaddr(current, arg, ARPHRD_ETHER, &mac.0)?
                    }
                    InterfaceKind::Loopback => {
                        write_ifreq_hwaddr(current, arg, ARPHRD_LOOPBACK, &[])?
                    }
                    InterfaceKind::Tun => write_ifreq_hwaddr(current, arg, ARPHRD_NONE, &[])?,
                }
            }
            SIOCGIFMTU => {
                let mtu = read_ifreq_interface(current, arg)?.mtu as i32;
                write_ifreq_data(current, arg, &mtu.to_ne_bytes())?;
            }
            SIOCGIFMETRIC => {
                read_ifreq_interface(current, arg)?;
                write_ifreq_data(current, arg, &0i32.to_ne_bytes())?;
            }
            SIOCGIFMAP => {
                read_ifreq_interface(current, arg)?;
                write_ifreq_data(current, arg, &[0; 24])?;
            }
            // In the "can be done by all, return a value" read-only group with the
            // other SIOCGIF* getters, but dev_ifsioc_locked has no bonding master to
            // report: an unknown name is ENODEV (read_ifreq_interface) and a resolved
            // interface is EINVAL (Linux net/core/dev_ioctl.c dev_ifsioc_locked).
            SIOCGIFSLAVE => {
                read_ifreq_interface(current, arg)?;
                return Err(StarryError::InvalidInput);
            }
            SIOCGIFTXQLEN => {
                read_ifreq_interface(current, arg)?;
                let qlen_ptr = (arg + offset_of!(ifreq, ifr_ifru)) as *mut i32;
                qlen_ptr.vm_write(current, 1000)?;
            }
            SIOCGIFINDEX => {
                let idx = read_ifreq_interface(current, arg)?.id.get() as i32;
                write_ifreq_data(current, arg, &idx.to_ne_bytes())?;
            }
            // Link speed/duplex query. No PHY is emulated, so report "not supported" the way a
            // virtual NIC (loopback, tun/tap) does. Tools like psutil's net_if_stats() treat
            // EOPNOTSUPP as "no ethtool" and degrade gracefully; any other errno makes them abort
            // the whole interface-status probe. Resolve the interface first so an unknown name
            // yields ENODEV, then fault on a bad ifr_data pointer, keeping Linux's error priority
            // (ENODEV, then EFAULT, then EOPNOTSUPP) and parity with the sibling SIOC*IF* arms.
            SIOCETHTOOL => {
                read_ifreq_interface(current, arg)?;
                let data_ptr = usize::from_ne_bytes(read_user_bytes::<8>(
                    current,
                    (arg + IFREQ_DATA_OFFSET) as *const u8,
                )?);
                read_user_bytes::<4>(current, data_ptr as *const u8)?;
                return Err(StarryError::OperationNotSupported);
            }
            _ => return Err(StarryError::NotATty),
        }
        Ok(0)
    })();
    match result {
        Err(StarryError::NotATty) => None,
        other => Some(other),
    }
}

/// `dev_ioctl` and `devinet_ioctl` require `CAP_NET_ADMIN` for changes.
fn require_net_admin(current: &crate::task::UserTaskRef) -> StarryResult<()> {
    if current.as_thread().cred().has_cap(CAP_NET_ADMIN) {
        Ok(())
    } else {
        Err(StarryError::OperationNotPermitted)
    }
}

fn read_ifreq_sockaddr(current: &crate::task::UserTaskRef, arg: usize) -> StarryResult<[u8; 16]> {
    read_user_bytes::<16>(current, (arg + IFREQ_DATA_OFFSET) as *const u8)
}

/// The address of a `sockaddr_in`; `devinet_ioctl` rejects other families.
fn sockaddr_in_address(sockaddr: &[u8; 16]) -> StarryResult<[u8; 4]> {
    if u16::from_ne_bytes([sockaddr[0], sockaddr[1]]) != AF_INET as u16 {
        return Err(StarryError::InvalidInput);
    }
    Ok([sockaddr[4], sockaddr[5], sockaddr[6], sockaddr[7]])
}

/// `inet_abc_len`: the classful prefix of an address, `None` for class D.
fn classful_prefix(ip: [u8; 4]) -> Option<u8> {
    let address = u32::from_be_bytes(ip);
    if address & 0xff00_0000 == 0 || address == u32::MAX {
        Some(0)
    } else if address & 0x8000_0000 == 0 {
        Some(8)
    } else if address & 0xc000_0000 == 0x8000_0000 {
        Some(16)
    } else if address & 0xe000_0000 == 0xc000_0000 {
        Some(24)
    } else if address & 0xf000_0000 == 0xf000_0000 {
        Some(32)
    } else {
        None
    }
}

/// `bad_mask` and `inet_mask_len`: the prefix of a contiguous mask that leaves
/// the host part of `address` clear.
fn mask_prefix(mask: [u8; 4], address: [u8; 4]) -> Option<u8> {
    let host = !u32::from_be_bytes(mask);
    (u32::from_be_bytes(address) & host == 0 && host & host.wrapping_add(1) == 0)
        .then(|| host.leading_zeros() as u8)
}

/// `SIOCSIFADDR` in `devinet_ioctl`: replaces the address, taking a classful
/// prefix, or /32 on a point-to-point link.
fn set_interface_address(info: &InterfaceInfo, ip: [u8; 4]) -> StarryResult<()> {
    let classful = classful_prefix(ip).ok_or(StarryError::InvalidInput)?;
    if let Some(current) = info.ipv4 {
        let current = current.address;
        if current.address().octets() == ip {
            return Ok(());
        }
        let address = core::net::Ipv4Addr::from(current.address().octets());
        ax_net::remove_interface_ipv4(info.id, address, current.prefix_len())?;
    }
    let address = core::net::Ipv4Addr::from(ip);
    // `inet_insert_ifa` discards an all-zero local address.
    if address.is_unspecified() {
        return Ok(());
    }
    let prefix = if info.flags.contains(InterfaceFlags::POINTOPOINT) {
        32
    } else {
        classful
    };
    Ok(ax_net::set_interface_ipv4(info.id, address, prefix)?)
}

/// `SIOCSIFNETMASK` in `devinet_ioctl`.
fn set_interface_netmask(info: &InterfaceInfo, mask: [u8; 4]) -> StarryResult<()> {
    let current = info
        .ipv4
        .ok_or(StarryError::from(Errno::EADDRNOTAVAIL))?
        .address;
    let prefix = mask_prefix(mask, [0; 4]).ok_or(StarryError::InvalidInput)?;
    if prefix == current.prefix_len() {
        return Ok(());
    }
    let address = core::net::Ipv4Addr::from(current.address().octets());
    ax_net::remove_interface_ipv4(info.id, address, current.prefix_len())?;
    Ok(ax_net::set_interface_ipv4(info.id, address, prefix)?)
}

/// `SIOCADDRT` and `SIOCDELRT`: `ip_rt_ioctl` over `rtentry_to_fib_config`.
fn write_route(current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> StarryResult<()> {
    let rt = read_user_bytes::<RTENTRY_LEN>(current, arg as *const u8)?;
    require_net_admin(current)?;
    let family = |offset: usize| u16::from_ne_bytes([rt[offset], rt[offset + 1]]);
    let address = |offset: usize| [rt[offset + 4], rt[offset + 5], rt[offset + 6], rt[offset + 7]];
    if family(RT_DST) != AF_INET as u16 {
        return Err(Errno::EAFNOSUPPORT.into());
    }
    let destination = address(RT_DST);
    let flags = u16::from_ne_bytes([rt[RT_FLAGS], rt[RT_FLAGS + 1]]);
    let prefix = if flags & RTF_HOST != 0 {
        32
    } else {
        let mask = address(RT_GENMASK);
        if family(RT_GENMASK) != AF_INET as u16 && (family(RT_GENMASK) != 0 || mask != [0; 4]) {
            return Err(Errno::EAFNOSUPPORT.into());
        }
        mask_prefix(mask, destination).ok_or(StarryError::InvalidInput)?
    };

    let device = usize::from_ne_bytes(core::array::from_fn(|index| rt[RT_DEV + index]));
    // Without `rt_dev` Linux derives the device from the gateway, which the
    // route table here cannot do.
    if device == 0 {
        return Err(StarryError::NoSuchDevice);
    }
    let name = read_user_bytes::<{ IFREQ_NAME_LEN - 1 }>(current, device as *const u8)?;
    let len = name.iter().position(|&byte| byte == 0).unwrap_or(name.len());
    // No interface carries alias labels, so a `name:label` device is absent.
    let info = core::str::from_utf8(&name[..len])
        .ok()
        .filter(|name| !name.contains(':'))
        .and_then(ax_net::interface_by_name)
        .filter(|info| in_root_net_ns() || info.kind == InterfaceKind::Loopback)
        .ok_or(StarryError::NoSuchDevice)?;

    let gateway = (family(RT_GATEWAY) == AF_INET as u16 && address(RT_GATEWAY) != [0; 4])
        .then(|| core::net::Ipv4Addr::from(address(RT_GATEWAY)));
    let destination = core::net::Ipv4Addr::from(destination);
    if cmd == SIOCDELRT {
        return ax_net::del_route(info.id, destination, prefix, gateway).map_err(|error| {
            match error {
                NetError::NotFound => Errno::ESRCH.into(),
                error => error.into(),
            }
        });
    }
    if flags & RTF_GATEWAY != 0 && gateway.is_none() {
        return Err(StarryError::InvalidInput);
    }
    // `fib_check_nh` refuses a next hop through a device that is down.
    if !info.flags.contains(InterfaceFlags::UP) {
        return Err(Errno::ENETDOWN.into());
    }
    Ok(ax_net::add_route(info.id, destination, prefix, gateway)?)
}

fn sockaddr_in_bytes(ip: [u8; 4]) -> [u8; 16] {
    let mut addr = [0; 16];
    addr[..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    addr[4..8].copy_from_slice(&ip);
    addr
}

fn write_ifreq_sockaddr(
    current: &crate::task::UserTaskRef,
    arg: usize,
    ip: [u8; 4],
) -> StarryResult<()> {
    write_ifreq_data(current, arg, &sockaddr_in_bytes(ip))
}

fn write_ifreq_hwaddr(
    current: &crate::task::UserTaskRef,
    arg: usize,
    hw_type: u16,
    hwaddr: &[u8],
) -> StarryResult<()> {
    let mut addr = [0; 16];
    addr[..2].copy_from_slice(&hw_type.to_ne_bytes());
    addr[2..2 + hwaddr.len()].copy_from_slice(hwaddr);
    write_ifreq_data(current, arg, &addr)
}

fn write_ifconf_entry(
    current: &crate::task::UserTaskRef,
    buf: usize,
    offset: usize,
    name: &str,
    ip: [u8; 4],
) -> StarryResult<()> {
    let mut ifreq = [0; IFREQ_COMPAT_LEN];
    let name = name.as_bytes();
    let name_len = name.len().min(IFREQ_NAME_LEN - 1);
    ifreq[..name_len].copy_from_slice(&name[..name_len]);
    ifreq[IFREQ_DATA_OFFSET..IFREQ_DATA_OFFSET + 16].copy_from_slice(&sockaddr_in_bytes(ip));
    Ok(vm_write_slice(current, (buf + offset) as *mut u8, &ifreq)?)
}

fn interface_ipv4(info: &InterfaceInfo) -> StarryResult<ax_net::Ipv4InterfaceConfig> {
    // `devinet_ioctl` answers a device without an IPv4 address this way.
    info.ipv4.ok_or(StarryError::from(Errno::EADDRNOTAVAIL))
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
    if info.flags.contains(InterfaceFlags::POINTOPOINT) {
        flags |= IFF_POINTOPOINT;
    }
    if info.flags.contains(InterfaceFlags::NOARP) {
        flags |= IFF_NOARP;
    }
    flags
}

fn write_ifconf(current: &crate::task::UserTaskRef, arg: usize) -> StarryResult<()> {
    let mut len = read_user_bytes::<4>(current, (arg + IFCONF_LEN_OFFSET) as *const u8)?;
    let ifc_len = i32::from_ne_bytes(len);
    let buf = usize::from_ne_bytes(read_user_bytes::<{ core::mem::size_of::<usize>() }>(
        current,
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
            write_ifconf_entry(current, buf, written, &name, ip)?;
            written += IFREQ_COMPAT_LEN;
        }
        len = (written as i32).to_ne_bytes();
    } else {
        len = ((interfaces.len() * IFREQ_COMPAT_LEN) as i32).to_ne_bytes();
    }
    vm_write_slice(current, (arg + IFCONF_LEN_OFFSET) as *mut u8, &len)?;
    Ok(())
}

impl Deref for Socket {
    type Target = SocketInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FileLike for Socket {
    fn validate_write_access(&self) -> StarryResult {
        Ok(())
    }

    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.recv_to_user(dst, RecvOptions::default())
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.send_from_user(
            src,
            Self::with_current_sender_credentials(SendOptions::default()),
        )
    }

    fn stat(&self) -> StarryResult<Kstat> {
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

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult<()> {
        Ok(self
            .inner
            .set_option(SetSocketOption::NonBlocking(&nonblocking))?)
    }

    fn async_mode(&self) -> bool {
        self.async_mode.load(Ordering::Acquire)
    }

    fn supports_async_mode(&self) -> bool {
        true
    }

    fn set_async_mode(&self, async_mode: bool) -> StarryResult {
        self.async_mode.store(async_mode, Ordering::Release);
        Ok(())
    }

    fn owner(&self) -> StarryResult<i32> {
        Ok(self.owner.load(Ordering::Acquire))
    }

    fn set_owner(&self, owner: i32) -> StarryResult {
        self.owner.store(owner, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        format!("socket:[{}]", self as *const _ as usize).into()
    }

    fn open_flags(&self) -> u32 {
        O_RDWR
    }

    fn ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> StarryResult<usize> {
        // Socket-specific query first, then the family-agnostic device ioctls
        // (SIOCGIF*), mirroring Linux sock_ioctl dispatching to dev_ioctl.
        if cmd == FIONREAD {
            let available = self.inner.recv_available()?.min(c_int::MAX as usize) as c_int;
            (arg as *mut c_int).vm_write(current, available)?;
            return Ok(0);
        }
        if let Some(result) = device_ioctl(current, cmd, arg) {
            return result;
        }
        if super::wext::is_wext_ioctl(cmd) {
            return super::wext::handle(current, cmd, arg);
        }
        Err(StarryError::NotATty)
    }

    fn from_fd(fd: c_int) -> StarryResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        get_file_like(fd)?
            .downcast_arc()
            .map_err(|_| StarryError::NotASocket)
    }
}

impl Pollable for Socket {
    fn poll(&self) -> IoEvents {
        self.inner.poll()
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.inner.register_shared(sink, events) };
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        unsafe { self.inner.register_exclusive(sink, events) };
    }
}
