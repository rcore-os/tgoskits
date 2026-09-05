use ax_net::SocketOps;
use linux_raw_sys::net::{sockaddr, socklen_t};

use super::addr::{SocketAddrExt, socket_addr_ex_for_user_name};
use crate::{
    file::{FileLike, PacketSocket, Socket, netlink::NetlinkSocket},
    mm::UserPtr,
};

pub fn sys_getsockname(
    current: &crate::task::UserTaskRef,
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> crate::StarryResult<isize> {
    let mut addrlen_value = addrlen.read(current)?;
    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let local_addr = packet.local_addr();
        local_addr.write_to_user(current, addr.as_ptr(), &mut addrlen_value)?;
        addrlen.write(current, addrlen_value)?;
        return Ok(0);
    }

    if let Ok(socket) = NetlinkSocket::from_fd(fd) {
        let local_addr = socket.local_addr();
        debug!("sys_getsockname <= fd: {fd}, netlink_addr: {local_addr:?}");
        super::addr::write_netlink_addr(current, &local_addr, addr, &mut addrlen_value)?;
        addrlen.write(current, addrlen_value)?;
        return Ok(0);
    }

    let socket = Socket::from_fd(fd)?;
    let local_addr = socket_addr_ex_for_user_name(socket.ip_domain(), socket.local_addr()?);
    debug!("sys_getsockname <= fd: {fd}, addr: {local_addr:?}");

    local_addr.write_to_user(current, addr, &mut addrlen_value)?;
    addrlen.write(current, addrlen_value)?;
    Ok(0)
}

pub fn sys_getpeername(
    current: &crate::task::UserTaskRef,
    fd: i32,
    addr: UserPtr<sockaddr>,
    addrlen: UserPtr<socklen_t>,
) -> crate::StarryResult<isize> {
    let mut addrlen_value = addrlen.read(current)?;
    let socket = Socket::from_fd(fd)?;
    let peer_addr = socket_addr_ex_for_user_name(socket.ip_domain(), socket.peer_addr()?);
    debug!("sys_getpeername <= fd: {fd}, addr: {peer_addr:?}");

    peer_addr.write_to_user(current, addr, &mut addrlen_value)?;
    addrlen.write(current, addrlen_value)?;
    Ok(0)
}
