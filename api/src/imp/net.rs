use core::net::SocketAddr;

use axerrno::{LinuxError, LinuxResult};
use axnet::{TcpSocket, UdpSocket};
use axsync::Mutex;
use linux_raw_sys::net::{AF_INET, IPPROTO_TCP, IPPROTO_UDP, SOCK_DGRAM, SOCK_STREAM, socklen_t};

use crate::{
    file::{FileLike, Socket},
    ptr::{UserConstPtr, UserPtr},
    sockaddr::SockAddr,
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

    socket
        .add_to_fd_table()
        .map(|fd| fd as isize)
        .map_err(|_| LinuxError::EMFILE)
}

fn to_socketaddr(addr: UserConstPtr<u8>, addrlen: u32) -> LinuxResult<SocketAddr> {
    let addr = addr.get_as_slice(addrlen as usize)?;
    let addr = unsafe { SockAddr::read(addr.as_ptr().cast(), addrlen)? };
    SocketAddr::try_from(addr)
}

pub fn sys_bind(fd: i32, addr: UserConstPtr<u8>, addrlen: u32) -> LinuxResult<isize> {
    let addr = to_socketaddr(addr, addrlen)?;
    debug!("sys_bind <= fd: {}, addr: {:?}", fd, addr);

    Socket::from_fd(fd)?.bind(addr)?;

    Ok(0)
}

pub fn sys_connect(fd: i32, addr: UserConstPtr<u8>, addrlen: u32) -> LinuxResult<isize> {
    let addr = to_socketaddr(addr, addrlen)?;
    debug!("sys_connect <= fd: {}, addr: {:?}", fd, addr);

    Socket::from_fd(fd)?.connect(addr)?;

    Ok(0)
}

pub fn sys_getsockname(
    fd: i32,
    addr: UserPtr<u8>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let local_addr = socket.local_addr()?;
    debug!("sys_getsockname <= fd: {}, addr: {:?}", fd, local_addr);

    let local_addr = SockAddr::from(local_addr);
    *addrlen.get_as_mut()? = local_addr.addr_len();
    let bytes = local_addr.bytes();
    addr.get_as_mut_slice(bytes.len())?.copy_from_slice(bytes);

    Ok(0)
}

pub fn sys_getpeername(
    fd: i32,
    addr: UserPtr<u8>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let peer_addr = socket.peer_addr()?;

    debug!("sys_getpeername <= fd: {}, addr: {:?}", fd, peer_addr);

    let peer_addr = SockAddr::from(peer_addr);
    *addrlen.get_as_mut()? = peer_addr.addr_len();
    let bytes = peer_addr.bytes();
    addr.get_as_mut_slice(bytes.len())?.copy_from_slice(bytes);

    Ok(0)
}

pub fn sys_listen(fd: i32, backlog: i32) -> LinuxResult<isize> {
    debug!("sys_listen: fd: {}, backlog: {}", fd, backlog);

    if backlog < 0 {
        return Err(LinuxError::EINVAL);
    }

    Socket::from_fd(fd)?.listen()?;

    Ok(0)
}

pub fn sys_accept(fd: i32, addr: UserPtr<u8>, addrlen: UserPtr<socklen_t>) -> LinuxResult<isize> {
    debug!("sys_accept <= fd: {}", fd);

    let socket = Socket::from_fd(fd)?;
    let socket = socket.accept()?;

    let remote_addr = socket.local_addr()?;
    let fd = socket
        .add_to_fd_table()
        .map(|fd| fd as isize)
        .map_err(|_| LinuxError::EMFILE)?;
    debug!("sys_accept => fd: {}, addr: {:?}", fd, remote_addr);

    let remote_addr = SockAddr::from(remote_addr);
    *addrlen.get_as_mut()? = remote_addr.addr_len();
    let bytes = remote_addr.bytes();
    addr.get_as_mut_slice(bytes.len())?.copy_from_slice(bytes);

    Ok(fd)
}

pub fn sys_sendto(
    fd: i32,
    buf: UserConstPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserConstPtr<u8>,
    addrlen: u32,
) -> LinuxResult<isize> {
    let addr = to_socketaddr(addr, addrlen)?;
    debug!(
        "sys_sendto <= fd: {}, len: {}, flags: {}, addr: {:?}",
        fd, len, flags, addr
    );

    let bytes = buf.get_as_slice(len)?;
    let socket = Socket::from_fd(fd)?;
    let sent = socket.sendto(bytes, addr)?;

    Ok(sent as isize)
}

pub fn sys_recvfrom(
    fd: i32,
    buf: UserPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserPtr<u8>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    debug!("sys_recvfrom <= fd: {}, len: {}, flags: {}", fd, len, flags);

    let socket = Socket::from_fd(fd)?;
    let buf = buf.get_as_mut_slice(len)?;
    let (recv, remote_addr) = socket.recvfrom(buf)?;

    if let Some(remote_addr) = remote_addr {
        let remote_addr = SockAddr::from(remote_addr);
        *addrlen.get_as_mut()? = remote_addr.addr_len();
        let bytes = remote_addr.bytes();
        addr.get_as_mut_slice(bytes.len())?.copy_from_slice(bytes);
    } else {
        *addrlen.get_as_mut()? = 0;
    }

    debug!("sys_recvfrom => fd: {}, recv: {}", fd, recv);
    Ok(recv as isize)
}
