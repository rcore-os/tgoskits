use alloc::boxed::Box;

#[cfg(feature = "vsock")]
use ax_net::vsock::VsockSocket;
use ax_net::{
    NetError, Shutdown, Socket as SocketInner, SocketAddrEx, SocketOps,
    raw::{IpProtocol, IpVersion, RawSocket},
    tcp::TcpSocket,
    udp::UdpSocket,
    unix::{DgramTransport, StreamTransport, UnixSocket, UnixSocketAddr},
};
use ax_task::current;
use axfs_ng_vfs::{MetadataUpdate, NodeType};
use linux_raw_sys::{
    general::{O_CLOEXEC, O_NONBLOCK},
    net::{
        AF_INET, AF_INET6, AF_NETLINK, AF_PACKET, AF_UNIX, AF_VSOCK, IPPROTO_ICMP, IPPROTO_TCP,
        IPPROTO_UDP, SHUT_RD, SHUT_RDWR, SHUT_WR, SOCK_DGRAM, SOCK_RAW, SOCK_SEQPACKET,
        SOCK_STREAM, sockaddr, socklen_t,
    },
    netlink::{NETLINK_GENERIC, NETLINK_KOBJECT_UEVENT, NETLINK_ROUTE},
};

use super::addr::{
    SocketAddrExt, normalize_socket_addr_ex_for_ip_stack, socket_addr_ex_for_user_name,
};
use crate::{
    Errno, StarryError, StarryResult,
    file::{FileLike, PacketSocket, SockAddrLl, Socket, add_file_like, netlink::NetlinkSocket},
    mm::{UserConstPtr, UserPtr},
    task::AsThread,
};

const SOCK_TYPE_MASK: u32 = 0xf;

const SOCK_MAX: u32 = 11;

const SOCK_FLAGS_MASK: u32 = O_NONBLOCK | O_CLOEXEC;

pub fn sys_socket(domain: u32, raw_ty: u32, proto: u32) -> StarryResult<isize> {
    debug!("sys_socket <= domain: {domain}, ty: {raw_ty}, proto: {proto}");
    let ty = raw_ty & SOCK_TYPE_MASK;
    if raw_ty & !(SOCK_TYPE_MASK | SOCK_FLAGS_MASK) != 0 || ty >= SOCK_MAX {
        return Err(StarryError::InvalidInput);
    }

    if domain == AF_PACKET {
        if ty != SOCK_DGRAM {
            warn!("Unsupported packet socket type: {ty}");
            return Err(StarryError::from(Errno::ESOCKTNOSUPPORT));
        }
        if !current().as_thread().cred().has_cap_net_raw() {
            return Err(StarryError::from(Errno::EPERM));
        }
        let socket = PacketSocket::new(proto as u16)?;
        if raw_ty & O_NONBLOCK != 0 {
            socket.set_nonblocking(true)?;
        }
        let cloexec = raw_ty & O_CLOEXEC != 0;
        return socket.add_to_fd_table(cloexec).map(|fd| fd as isize);
    }

    let unix_credentials = Socket::current_unix_credentials();
    let ip_domain = if domain == AF_INET || domain == AF_INET6 {
        domain
    } else {
        AF_INET
    };

    let socket = match (domain, ty) {
        (AF_INET | AF_INET6, SOCK_STREAM) => {
            if proto != 0 && proto != IPPROTO_TCP as _ {
                return Err(StarryError::from(Errno::EPROTONOSUPPORT));
            }
            TcpSocket::new().into()
        }
        (AF_INET, SOCK_DGRAM) if proto == IPPROTO_ICMP as u32 => {
            // StarryOS does not expose Linux's per-network-namespace
            // `ping_group_range` policy yet, so keep ping sockets behind the
            // existing CAP_NET_RAW capability boundary.
            if !current().as_thread().cred().has_cap_net_raw() {
                return Err(StarryError::from(Errno::EPERM));
            }
            SocketInner::Raw(Box::new(RawSocket::new_ipv4_ping()))
        }
        (AF_INET | AF_INET6, SOCK_DGRAM) => {
            if proto != 0 && proto != IPPROTO_UDP as _ {
                return Err(StarryError::from(Errno::EPROTONOSUPPORT));
            }
            UdpSocket::new().into()
        }
        (AF_UNIX, SOCK_STREAM) => UnixSocket::new(StreamTransport::new(unix_credentials)).into(),
        (AF_UNIX, SOCK_DGRAM) => UnixSocket::new(DgramTransport::new(unix_credentials)).into(),
        (AF_UNIX, SOCK_SEQPACKET) => {
            UnixSocket::new(DgramTransport::new_seqpacket(unix_credentials)).into()
        }
        (AF_NETLINK, SOCK_RAW) | (AF_NETLINK, SOCK_DGRAM) => {
            match proto {
                NETLINK_KOBJECT_UEVENT | NETLINK_ROUTE | NETLINK_GENERIC => {}
                _ => return Err(StarryError::from(Errno::EPROTONOSUPPORT)),
            }
            if proto == NETLINK_KOBJECT_UEVENT && ty != SOCK_RAW {
                return Err(StarryError::from(Errno::ESOCKTNOSUPPORT));
            }
            let socket = NetlinkSocket::new(proto, ty);
            if raw_ty & O_NONBLOCK != 0 {
                socket.set_nonblocking(true)?;
            }
            let cloexec = raw_ty & O_CLOEXEC != 0;
            return add_file_like(socket as _, cloexec).map(|fd| fd as isize);
        }
        #[cfg(feature = "vsock")]
        (AF_VSOCK, SOCK_STREAM) => VsockSocket::new().into(),
        (AF_INET, SOCK_RAW) => {
            if proto != IPPROTO_ICMP as u32 {
                return Err(StarryError::from(Errno::EPROTONOSUPPORT));
            }
            if !current().as_thread().cred().has_cap_net_raw() {
                return Err(StarryError::from(Errno::EPERM));
            }
            SocketInner::Raw(Box::new(RawSocket::new(IpVersion::Ipv4, IpProtocol::Icmp)))
        }
        (AF_INET | AF_INET6, _) | (AF_UNIX, _) | (AF_NETLINK, _) | (AF_VSOCK, _) => {
            warn!("Unsupported socket type: domain: {domain}, ty: {ty}");
            return Err(StarryError::from(Errno::ESOCKTNOSUPPORT));
        }
        _ => {
            return Err(StarryError::from(Errno::EAFNOSUPPORT));
        }
    };
    let socket = Socket::new(socket, ip_domain);

    if raw_ty & O_NONBLOCK != 0 {
        socket.set_nonblocking(true)?;
    }
    let cloexec = raw_ty & O_CLOEXEC != 0;

    socket.add_to_fd_table(cloexec).map(|fd| fd as isize)
}

pub fn sys_bind(fd: i32, addr: UserConstPtr<sockaddr>, addrlen: u32) -> StarryResult<isize> {
    if let Ok(socket) = NetlinkSocket::from_fd(fd) {
        let mut addr = super::addr::read_netlink_addr(addr, addrlen as _)?;
        if addr.nl_pid == 0 {
            let thread = current();
            let thread = thread.as_thread();
            addr.nl_pid = crate::task::current_pid_view()
                .visible_number(&thread.proc_data.identity())
                .expect("current process is visible in its active PID namespace")
                .get();
        }
        debug!("sys_bind <= fd: {fd}, netlink_addr: {addr:?}");
        socket.bind(addr)?;
        return Ok(0);
    }

    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let addr =
            SockAddrLl::read_from_user(addr.address().as_usize() as *const sockaddr, addrlen)?;
        packet.bind_ll(addr)?;
        return Ok(0);
    }

    let socket = Socket::from_fd(fd)?;
    let mut addr = SocketAddrEx::read_from_user(addr, addrlen)?;
    if socket.ip_domain() == AF_INET6 {
        addr = normalize_socket_addr_ex_for_ip_stack(addr, true)?;
    }
    debug!("sys_bind <= fd: {fd}, addr: {addr:?}");

    let unix_path = match &addr {
        SocketAddrEx::Unix(UnixSocketAddr::Path(path)) => Some(path.clone()),
        _ => None,
    };
    let cred = current().as_thread().cred();

    socket.bind(addr)?;

    if let Some(path) = unix_path
        && let Err(err) = ax_fs_ng::vfs::current_fs_context()
            .lock()
            .resolve_no_follow(path.as_ref())
            .and_then(|loc| {
                if loc.metadata()?.node_type == NodeType::Socket {
                    loc.update_metadata(MetadataUpdate {
                        owner: Some((cred.fsuid, cred.fsgid)),
                        ..Default::default()
                    })?;
                }
                Ok(())
            })
    {
        warn!("failed to update AF_UNIX socket owner for {path}: {err:?}");
    }

    Ok(0)
}

pub fn sys_connect(fd: i32, addr: UserConstPtr<sockaddr>, addrlen: u32) -> StarryResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let mut addr = SocketAddrEx::read_from_user(addr, addrlen)?;
    if socket.ip_domain() == AF_INET6 {
        addr = normalize_socket_addr_ex_for_ip_stack(addr, false)?;
    }
    debug!("sys_connect <= fd: {fd}, addr: {addr:?}");

    socket.connect(addr).map_err(|e| {
        if matches!(e, NetError::WouldBlock) {
            StarryError::InProgress
        } else {
            e.into()
        }
    })?;

    Ok(0)
}

pub fn sys_listen(fd: i32, backlog: i32) -> StarryResult<isize> {
    debug!("sys_listen <= fd: {fd}, backlog: {backlog}");

    if backlog < 0 && backlog != -1 {
        return Err(StarryError::InvalidInput);
    }

    Socket::from_fd(fd)?.listen(backlog as usize)?;

    Ok(0)
}

pub fn sys_accept(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> StarryResult<isize> {
    sys_accept4(fd, addr, addrlen, 0)
}

pub fn sys_accept4(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
    flags: u32,
) -> StarryResult<isize> {
    debug!("sys_accept <= fd: {fd}, flags: {flags}");

    // accept4 only accepts SOCK_CLOEXEC / SOCK_NONBLOCK (== O_CLOEXEC / O_NONBLOCK);
    // any other bit is EINVAL (Linux net/socket.c __sys_accept4).
    if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
        return Err(StarryError::InvalidInput);
    }

    let cloexec = flags & O_CLOEXEC != 0;

    let listener = Socket::from_fd(fd)?;
    let socket = Socket::new(listener.accept()?, listener.ip_domain());
    if flags & O_NONBLOCK != 0 {
        socket.set_nonblocking(true)?;
    }

    let remote_addr = socket_addr_ex_for_user_name(socket.ip_domain(), socket.peer_addr()?);
    let fd = socket.add_to_fd_table(cloexec).map(|fd| fd as isize)?;
    debug!("sys_accept => fd: {fd}, addr: {remote_addr:?}");

    if !addr.is_null() {
        remote_addr.write_to_user(addr, addrlen.get_as_mut()?)?;
    }

    Ok(fd)
}

pub fn sys_shutdown(fd: i32, how: u32) -> StarryResult<isize> {
    debug!("sys_shutdown <= fd: {fd}, how: {how:?}");

    let socket = Socket::from_fd(fd)?;
    let how = match how {
        SHUT_RD => Shutdown::Read,
        SHUT_WR => Shutdown::Write,
        SHUT_RDWR => Shutdown::Both,
        _ => return Err(StarryError::InvalidInput),
    };
    Ok(socket.shutdown(how).map(|_| 0)?)
}

pub fn sys_socketpair(
    domain: u32,
    raw_ty: u32,
    proto: u32,
    fds: UserPtr<[i32; 2]>,
) -> StarryResult<isize> {
    debug!("sys_socketpair <= domain: {domain}, ty: {raw_ty}, proto: {proto}");
    let ty = raw_ty & 0xFF;

    if domain != AF_UNIX {
        return Err(StarryError::from(Errno::EAFNOSUPPORT));
    }

    let credentials = Socket::current_unix_credentials();
    let (sock1, sock2) = match ty {
        SOCK_STREAM => {
            let (sock1, sock2) = StreamTransport::new_pair(credentials);
            (UnixSocket::new(sock1), UnixSocket::new(sock2))
        }
        SOCK_DGRAM => {
            let (sock1, sock2) = DgramTransport::new_pair(credentials);
            (UnixSocket::new(sock1), UnixSocket::new(sock2))
        }
        SOCK_SEQPACKET => {
            let (sock1, sock2) = DgramTransport::new_pair_seqpacket(credentials);
            (UnixSocket::new(sock1), UnixSocket::new(sock2))
        }
        _ => {
            warn!("Unsupported socketpair type: {ty}");
            return Err(StarryError::from(Errno::ESOCKTNOSUPPORT));
        }
    };
    let sock1 = Socket::new(sock1.into(), AF_UNIX);
    let sock2 = Socket::new(sock2.into(), AF_UNIX);

    if raw_ty & O_NONBLOCK != 0 {
        sock1.set_nonblocking(true)?;
        sock2.set_nonblocking(true)?;
    }
    let cloexec = raw_ty & O_CLOEXEC != 0;

    *fds.get_as_mut()? = [
        sock1.add_to_fd_table(cloexec)?,
        sock2.add_to_fd_table(cloexec)?,
    ];
    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn net_socket_constants_hold_for_test() -> bool {
    const {
        assert!(AF_INET == 2);
        assert!(AF_INET6 == 10);
        assert!(AF_UNIX == 1);
        assert!(AF_NETLINK == 16);
        assert!(AF_PACKET == 17);

        assert!(SOCK_STREAM == 1);
        assert!(SOCK_DGRAM == 2);
        assert!(SOCK_RAW == 3);
        assert!(SOCK_SEQPACKET == 5);

        assert!(SHUT_RD == 0);
        assert!(SHUT_WR == 1);
        assert!(SHUT_RDWR == 2);
    }

    #[cfg(feature = "vsock")]
    const {
        assert!(AF_VSOCK == 40);
    }

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn net_socket_constants_hold() {
        assert!(super::net_socket_constants_hold_for_test());
    }
}
