use core::net::Ipv4Addr;

use axerrno::LinuxResult;
use axio::buf::BufMut;
use axnet::{RecvFlags, SendFlags, SocketAddrEx, SocketOps};
use linux_raw_sys::{
    general::iovec,
    net::{MSG_PEEK, MSG_TRUNC, msghdr, sockaddr, socklen_t},
};

use crate::{
    file::{FileLike, Socket},
    io::IoVectorBuf,
    mm::{UserConstPtr, UserPtr},
    socket::SocketAddrExt,
};

pub fn sys_sendto(
    fd: i32,
    buf: UserConstPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserConstPtr<sockaddr>,
    addrlen: u32,
) -> LinuxResult<isize> {
    let addr = if addr.is_null() || addrlen == 0 {
        None
    } else {
        Some(SocketAddrEx::read_from_user(addr, addrlen)?)
    };

    debug!(
        "sys_sendto <= fd: {}, len: {}, flags: {}, addr: {:?}",
        fd, len, flags, addr
    );

    let bytes = buf.get_as_slice(len)?;
    let socket = Socket::from_fd(fd)?;
    let sent = socket.send(&mut &*bytes, addr, SendFlags::empty())?;

    Ok(sent as isize)
}

fn recv_impl(
    fd: i32,
    mut dst: impl BufMut,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    debug!("sys_recv <= fd: {}, flags: {}", fd, flags);

    let socket = Socket::from_fd(fd)?;
    let mut recv_flags = RecvFlags::empty();
    if flags & MSG_PEEK != 0 {
        recv_flags |= RecvFlags::PEEK;
    }
    if flags & MSG_TRUNC != 0 {
        recv_flags |= RecvFlags::TRUNCATE;
    }

    let mut remote_addr =
        (!addr.is_null()).then(|| SocketAddrEx::Ip((Ipv4Addr::UNSPECIFIED, 0).into()));
    let recv = socket.recv(&mut dst, remote_addr.as_mut(), recv_flags)?;

    if let Some(remote_addr) = remote_addr {
        remote_addr.write_to_user(addr, addrlen.get_as_mut()?)?;
    }

    debug!("sys_recv => fd: {}, recv: {}", fd, recv);
    Ok(recv as isize)
}

pub fn sys_recvfrom(
    fd: i32,
    buf: UserPtr<u8>,
    len: usize,
    flags: u32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> LinuxResult<isize> {
    recv_impl(fd, buf.get_as_mut_slice(len)?, flags, addr, addrlen)
}

pub fn sys_recvmsg(fd: i32, msg: UserPtr<msghdr>, flags: u32) -> LinuxResult<isize> {
    let msg = msg.get_as_mut()?;
    recv_impl(
        fd,
        IoVectorBuf::new_mut(UserPtr::from(msg.msg_iov as *mut iovec), msg.msg_iovlen)?,
        flags,
        UserPtr::from(msg.msg_name as usize),
        UserPtr::from(&mut msg.msg_namelen as *mut _ as *mut socklen_t),
    )
}
