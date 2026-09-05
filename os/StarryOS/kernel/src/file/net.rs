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
    ConnectStatus, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind, NetError,
    RecvFlags, RecvOptions, SendFlags, SendOptions, Socket as SocketInner, SocketAddrEx, SocketOps,
    SocketWaitPolicy, poll_socket_io,
    options::{Configurable, GetSocketOption, SetSocketOption, UnixCredentials},
};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::{
    general::{CAP_NET_ADMIN, O_RDWR, S_IFSOCK},
    ioctl::{
        FIONREAD, SIOCGIFADDR, SIOCGIFBRDADDR, SIOCGIFCONF, SIOCGIFDSTADDR, SIOCGIFFLAGS,
        SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMAP, SIOCGIFMETRIC, SIOCGIFMTU, SIOCGIFNETMASK,
        SIOCGIFSLAVE, SIOCGIFTXQLEN, SIOCSIFFLAGS,
    },
    net::{AF_INET, ifreq},
};

use super::{FileLike, Kstat};
use crate::{
    StarryError, StarryResult,
    file::{IoDst, IoSrc, get_file_like},
    mm::{VmMutPtr, vm_read_slice, vm_write_slice},
    task::{
        UserTaskRef, current_pid_view, current_user_task,
        future::{UserWaitOutcome, block_on_user_timeout},
    },
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
// ethtool ioctl; not exported by linux-raw-sys. The value is arch-independent.
const SIOCETHTOOL: u32 = 0x8946;
// Map an interface index to its name (Linux net/core/dev_ioctl.c dev_ifname).
// Arch-independent; the inverse of SIOCGIFINDEX.
const SIOCGIFNAME: u32 = 0x8910;
const IFCONF_LEN_OFFSET: usize = 0;
const IFCONF_BUF_OFFSET: usize = 8;
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
                let info = read_ifreq_interface(current, arg)?;
                if !current.as_thread().cred().has_cap(CAP_NET_ADMIN) {
                    return Err(StarryError::OperationNotPermitted);
                }
                if read_ifreq_flags(current, arg)? != linux_flags(&info) {
                    return Err(StarryError::OperationNotSupported);
                }
            }
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
                let addr = if info.kind == InterfaceKind::Loopback {
                    interface_ipv4(&info)?.address.address().octets()
                } else {
                    [0, 0, 0, 0]
                };
                write_ifreq_sockaddr(current, arg, addr)?;
            }
            SIOCGIFBRDADDR => {
                let info = read_ifreq_interface(current, arg)?;
                let addr = if info.kind == InterfaceKind::Loopback {
                    interface_ipv4(&info)?.address.address().octets()
                } else {
                    ipv4_broadcast(interface_ipv4(&info)?)
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
                    InterfaceKind::Ethernet => {
                        let mac = info.mac.ok_or(StarryError::NoSuchDevice)?;
                        write_ifreq_hwaddr(current, arg, ARPHRD_ETHER, &mac.0)?
                    }
                    InterfaceKind::Loopback => {
                        write_ifreq_hwaddr(current, arg, ARPHRD_LOOPBACK, &[])?
                    }
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
    info.ipv4.ok_or(StarryError::NoSuchDeviceOrAddress)
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
