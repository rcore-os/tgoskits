use core::net::SocketAddr;

use axerrno::{LinuxError, LinuxResult};
use axnet::{TcpSocket, UdpSocket};
use axsync::Mutex;
use linux_raw_sys::net::{
    AF_INET, IPPROTO_TCP, IPPROTO_UDP, SOCK_DGRAM, SOCK_STREAM, sockaddr, socklen_t,
};

use crate::{
    file::{FileLike, Socket},
    ptr::{UserConstPtr, UserPtr, nullable},
    socket::SocketAddrExt,
};

pub fn sys_socket(domain: u32, ty: u32, proto: u32) -> LinuxResult<isize> {
    let ty = ty & 0xFF;

    debug!(
        "sys_socket <= domain: {}, ty: {}, proto: {}",
        domain, ty, proto
    );

    if domain != AF_INET {
        return Err(LinuxError::EAFNOSUPPORT);
    }

    let socket = match ty {
        SOCK_STREAM => {
            if proto != 0 && proto != IPPROTO_TCP as _ {
                return Err(LinuxError::EPROTONOSUPPORT);
            }
            Socket::Tcp(Mutex::new(TcpSocket::new()))
        }
        SOCK_DGRAM => {
            if proto != 0 && proto != IPPROTO_UDP as _ {
                return Err(LinuxError::EPROTONOSUPPORT);
            }
            Socket::Udp(Mutex::new(UdpSocket::new()))
        }
        _ => return Err(LinuxError::ESOCKTNOSUPPORT),
    };

    socket.add_to_fd_table().map(|fd| fd as isize)
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

    Socket::from_fd(fd)?.connect(addr)?;

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
    debug!("sys_accept <= fd: {}", fd);

    let socket = Socket::from_fd(fd)?;
    let socket = socket.accept()?;

    let remote_addr = socket.local_addr()?;
    let fd = socket.add_to_fd_table().map(|fd| fd as isize)?;
    debug!("sys_accept => fd: {}, addr: {:?}", fd, remote_addr);

    if !addr.is_null() {
        let len = remote_addr.write_to_user(addr)?;
        if let Some(addrlen) = nullable!(addrlen.get_as_mut())? {
            *addrlen = len;
        }
    }

    Ok(fd)
}
