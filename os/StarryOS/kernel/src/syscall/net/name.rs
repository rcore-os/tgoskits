use ax_net::SocketOps;
use linux_raw_sys::net::{sockaddr, socklen_t};
use starry_vm::{VmMutPtr, VmPtr};

use super::addr::{SocketAddrExt, socket_addr_ex_for_user_name};
use crate::{
    StarryResult,
    file::{FileLike, PacketSocket, Socket, netlink::NetlinkSocket},
};

pub fn sys_getsockname(
    fd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> StarryResult<isize> {
    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let local_addr = packet.local_addr();
        let mut output_len = addrlen.vm_read()?;
        local_addr.write_to_user(addr, &mut output_len)?;
        addrlen.vm_write(output_len)?;
        return Ok(0);
    }

    if let Ok(socket) = NetlinkSocket::from_fd(fd) {
        let local_addr = socket.local_addr();
        debug!("sys_getsockname <= fd: {fd}, netlink_addr: {local_addr:?}");
        let mut output_len = addrlen.vm_read()?;
        super::addr::write_netlink_addr(&local_addr, addr, &mut output_len)?;
        addrlen.vm_write(output_len)?;
        return Ok(0);
    }

    let socket = Socket::from_fd(fd)?;
    let local_addr = socket_addr_ex_for_user_name(socket.ip_domain(), socket.local_addr()?);
    debug!("sys_getsockname <= fd: {fd}, addr: {local_addr:?}");

    let mut output_len = addrlen.vm_read()?;
    local_addr.write_to_user(addr, &mut output_len)?;
    addrlen.vm_write(output_len)?;
    Ok(0)
}

pub fn sys_getpeername(
    fd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> StarryResult<isize> {
    let socket = Socket::from_fd(fd)?;
    let peer_addr = socket_addr_ex_for_user_name(socket.ip_domain(), socket.peer_addr()?);
    debug!("sys_getpeername <= fd: {fd}, addr: {peer_addr:?}");

    let mut output_len = addrlen.vm_read()?;
    peer_addr.write_to_user(addr, &mut output_len)?;
    addrlen.vm_write(output_len)?;
    Ok(0)
}
