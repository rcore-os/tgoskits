use core::net::SocketAddr;

use axerrno::{LinuxError, LinuxResult};
use axnet::{Shutdown, SocketOps, TcpSocket, UdpSocket};
use linux_raw_sys::{
    general::O_NONBLOCK,
    net::{
        AF_INET, AF_UNIX, IPPROTO_TCP, IPPROTO_UDP, SHUT_RD, SHUT_RDWR, SHUT_WR, SOCK_DGRAM,
        SOCK_STREAM, sockaddr, socklen_t,
    },
};

use crate::{
    file::{FileLike, Socket},
    mm::{UserConstPtr, UserPtr, nullable},
    socket::SocketAddrExt,
};

pub fn sys_socket(domain: u32, raw_ty: u32, proto: u32) -> LinuxResult<isize> {
    debug!(
        "sys_socket <= domain: {}, ty: {}, proto: {}",
        domain, raw_ty, proto
    );
    let ty = raw_ty & 0xFF;

    // FIXME: unix domain socket
    if domain != AF_INET && domain != AF_UNIX {
        return Err(LinuxError::EAFNOSUPPORT);
    }

    let socket = match ty {
        SOCK_STREAM => {
            if proto != 0 && proto != IPPROTO_TCP as _ {
                return Err(LinuxError::EPROTONOSUPPORT);
            }
            Socket(axnet::Socket::Tcp(TcpSocket::new()))
        }
        SOCK_DGRAM => {
            if proto != 0 && proto != IPPROTO_UDP as _ {
                return Err(LinuxError::EPROTONOSUPPORT);
            }
            Socket(axnet::Socket::Udp(UdpSocket::new()))
        }
        _ => return Err(LinuxError::ESOCKTNOSUPPORT),
    };
    if raw_ty & O_NONBLOCK != 0 {
        socket.set_nonblocking(true)?;
    }

    socket.add_to_fd_table(false).map(|fd| fd as isize)
}

pub fn sys_bind(fd: i32, addr: UserConstPtr<sockaddr>, addrlen: u32) -> LinuxResult<isize> {
    let addr = SocketAddr::read_from_user(addr, addrlen)?;
    debug!("sys_bind <= fd: {}, addr: {:?}", fd, addr);

    Socket::from_fd(fd)?.bind(addr)?;

    Ok(0)
}

pub fn sys_connect(fd: i32, addr: UserConstPtr<sockaddr>, addrlen: u32) -> LinuxResult<isize> {
    let addr = SocketAddr::read_from_user(addr, addrlen)?;
    debug!("sys_connect <= fd: {}, addr: {:?}", fd, addr);

    Socket::from_fd(fd)?.connect(addr).map_err(|e| {
        if e == LinuxError::EAGAIN {
            LinuxError::EINPROGRESS
        } else {
            e
        }
    })?;

    Ok(0)
}

pub fn sys_listen(fd: i32, backlog: i32) -> LinuxResult<isize> {
    debug!("sys_listen <= fd: {}, backlog: {}", fd, backlog);

    if backlog < 0 {
        return Err(LinuxError::EINVAL);
    }

    Socket::from_fd(fd)?.listen()?;

    Ok(0)
}

pub fn sys_accept(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    sys_accept4(fd, addr, addrlen, 0)
}

pub fn sys_accept4(
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
    flags: i32,
) -> LinuxResult<isize> {
    debug!("sys_accept <= fd: {}, flags: {}", fd, flags);

    let socket = Socket::from_fd(fd)?;
    let socket = Socket(socket.accept()?);

    let remote_addr = socket.local_addr()?;
    let fd = socket.add_to_fd_table(false).map(|fd| fd as isize)?;
    debug!("sys_accept => fd: {}, addr: {:?}", fd, remote_addr);

    if !addr.is_null() {
        let len = remote_addr.write_to_user(addr)?;
        if let Some(addrlen) = nullable!(addrlen.get_as_mut())? {
            *addrlen = len;
        }
    }

    Ok(fd)
}

pub fn sys_shutdown(fd: i32, how: u32) -> LinuxResult<isize> {
    debug!("sys_shutdown <= fd: {}, how: {:?}", fd, how);

    let socket = Socket::from_fd(fd)?;
    let how = match how {
        SHUT_RD => Shutdown::Read,
        SHUT_WR => Shutdown::Write,
        SHUT_RDWR => Shutdown::Both,
        _ => return Err(LinuxError::EINVAL),
    };
    socket.shutdown(how).map(|_| 0)
}
