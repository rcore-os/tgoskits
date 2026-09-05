use alloc::{boxed::Box, vec::Vec};
use core::{net::Ipv4Addr, time::Duration};

use ax_io::prelude::*;
use ax_net::{
    CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, SocketAddrEx, SocketCmsg,
    SocketOps,
};
use ax_runtime::hal::time::monotonic_time;
use linux_raw_sys::{
    general::{timespec, timeval},
    net::{
        IP_TOS, IP_TTL, IPPROTO_IPV6, IPV6_TCLASS, MSG_CMSG_CLOEXEC, MSG_CTRUNC, MSG_DONTWAIT,
        MSG_OOB, MSG_PEEK, MSG_TRUNC, SCM_CREDENTIALS, SCM_TIMESTAMP, SOL_SOCKET, cmsghdr, mmsghdr,
        msghdr, sockaddr, socklen_t, ucred,
    },
};
use starry_vm::{VmMutPtr, VmPtr, vm_load};

use super::addr::{
    SocketAddrExt, normalize_socket_addr_ex_for_ip_stack, socket_addr_ex_for_user_name,
};
use crate::{
    StarryError, StarryResult,
    file::{FileLike, PacketSocket, Socket, get_file_like, netlink::NetlinkSocket},
    mm::{IoVec, IoVectorBuf, VmBytes, VmBytesMut},
    syscall::net::{CMsg, CMsgBuilder, cmsg_space},
    time::TimeValueLike,
};

// Linux ABI for sendmmsg/recvmmsg limits vlen to UIO_MAXIOV (1024).
const MMSG_MAX_VLEN: u32 = 1024;
// recvmmsg-only flag (uapi/linux/socket.h): after the first datagram is
// received, the remaining recvs behave as if MSG_DONTWAIT were set.
const MSG_WAITFORONE: u32 = 0x10000;
const PROTO_IP: u32 = linux_raw_sys::net::IPPROTO_IP as u32;

fn parse_recvmmsg_timeout(timeout: *const timespec) -> StarryResult<Option<Duration>> {
    if timeout.is_null() {
        return Ok(None);
    }
    // SAFETY: Linux `timespec` is made only of integer fields.
    let ts = unsafe { timeout.vm_read_any()? };
    let tv = ts.try_into_time_value()?;
    Ok(Some(Duration::new(tv.as_secs(), tv.subsec_nanos())))
}

fn parse_send_cmsgs(control_ptr: usize, control_len: usize) -> StarryResult<Vec<CMsgData>> {
    let mut cmsg = Vec::new();
    if control_ptr == 0 || control_len == 0 {
        return Ok(cmsg);
    }

    let mut ptr = control_ptr;
    let ptr_end = ptr
        .checked_add(control_len)
        .ok_or(StarryError::InvalidInput)?;

    while let Some(next) = ptr.checked_add(size_of::<cmsghdr>()) {
        if next > ptr_end {
            break;
        }

        // SAFETY: Linux `cmsghdr` is made only of integer fields.
        let hdr = unsafe { (ptr as *const cmsghdr).vm_read_any()? };
        if hdr.cmsg_len < size_of::<cmsghdr>() || ptr_end - ptr < hdr.cmsg_len {
            return Err(StarryError::InvalidInput);
        }

        let Some(next_ptr) = cmsg_space(hdr.cmsg_len - size_of::<cmsghdr>())
            .and_then(|space| ptr.checked_add(space))
        else {
            return Err(StarryError::InvalidInput);
        };

        let body = vm_load(
            (ptr + size_of::<cmsghdr>()) as *const u8,
            hdr.cmsg_len - size_of::<cmsghdr>(),
        )?;
        cmsg.push(Box::new(CMsg::parse(&hdr, &body)?) as CMsgData);
        ptr = next_ptr;
    }

    Ok(cmsg)
}

fn send_impl(
    fd: i32,
    mut src: impl Read + IoBuf,
    flags: u32,
    addr: *const sockaddr,
    addrlen: socklen_t,
    cmsg: Vec<CMsgData>,
) -> StarryResult<isize> {
    if let Ok(packet) = PacketSocket::from_fd(fd) {
        return Ok(packet.send_packet(&mut src)? as isize);
    }

    if let Ok(socket) = Socket::from_fd(fd) {
        let addr = if addr.is_null() {
            // addr == NULL: treat as no address regardless of addrlen.
            // Linux sendto(..., NULL, nonzero) sends to connected peer or
            // returns EDESTADDRREQ on unconnected socket, never EINVAL.
            None
        } else if addrlen == 0 {
            return Err(StarryError::InvalidInput);
        } else {
            let mut addr = SocketAddrEx::read_from_user(addr, addrlen)?;
            if socket.ip_domain() == linux_raw_sys::net::AF_INET6 {
                addr = normalize_socket_addr_ex_for_ip_stack(addr, false)?;
            }
            Some(addr)
        };

        let send_flags = SendFlags::from_bits_retain(flags);

        debug!("sys_send <= fd: {fd}, flags: {flags:#x}, addr: {addr:?}");

        let sent = socket.send(
            &mut src,
            Socket::with_current_sender_credentials(SendOptions {
                to: addr,
                flags: send_flags,
                cmsg,
                ..Default::default()
            }),
        )?;

        return Ok(sent as isize);
    }

    if let Ok(netlink) = NetlinkSocket::from_fd(fd) {
        let sent = netlink.write(&mut src)?;
        return Ok(sent as isize);
    }

    get_file_like(fd)?;
    Err(StarryError::NotASocket)
}

pub fn sys_sendto(
    fd: i32,
    buf: *const u8,
    len: usize,
    flags: u32,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> StarryResult<isize> {
    send_impl(fd, VmBytes::new(buf, len), flags, addr, addrlen, Vec::new())
}

pub fn sys_sendmsg(fd: i32, msg: *const msghdr, flags: u32) -> StarryResult<isize> {
    // SAFETY: Linux `msghdr` contains only integers and raw pointers.
    let msg = unsafe { msg.vm_read_any()? };
    let cmsg = parse_send_cmsgs(msg.msg_control as usize, msg.msg_controllen)?;
    send_impl(
        fd,
        IoVectorBuf::new(msg.msg_iov as *const IoVec, msg.msg_iovlen)?.into_io(),
        flags,
        msg.msg_name.cast(),
        msg.msg_namelen as socklen_t,
        cmsg,
    )
}

// Data-truncation and control-truncation are reported through separate out
// flags because they feed different sinks (one into RecvOptions, one set
// directly), so they stay as distinct parameters rather than a bundled struct.
#[derive(Clone, Copy)]
struct RecvName {
    addr: *mut sockaddr,
    capacity: RecvNameCapacity,
}

#[derive(Clone, Copy)]
enum RecvNameCapacity {
    Header(socklen_t),
    User(*const socklen_t),
}

impl RecvNameCapacity {
    fn read(self) -> StarryResult<socklen_t> {
        match self {
            Self::Header(capacity) => Ok(capacity),
            Self::User(pointer) => Ok(pointer.vm_read()?),
        }
    }
}

struct RecvOutcome {
    received: isize,
    name_len: Option<socklen_t>,
}

#[allow(clippy::too_many_arguments)]
fn recv_impl(
    fd: i32,
    mut dst: impl Write + IoBufMut,
    flags: u32,
    name: Option<RecvName>,
    mut cmsg_builder: Option<CMsgBuilder>,
    truncated_out: &mut bool,
    control_truncated_out: &mut bool,
) -> StarryResult<RecvOutcome> {
    debug!("sys_recv <= fd: {fd}, flags: {flags}");

    if let Ok(packet) = PacketSocket::from_fd(fd) {
        let (recv, from) = packet.recv_packet(&mut dst)?;
        let name_len = if let Some(name) = name {
            let mut len = name.capacity.read()?;
            from.write_to_user(name.addr, &mut len)?;
            Some(len)
        } else {
            None
        };
        if let Some(builder) = cmsg_builder.take() {
            builder.finish();
        }
        return Ok(RecvOutcome {
            received: recv as isize,
            name_len,
        });
    }

    let Ok(socket) = Socket::from_fd(fd) else {
        if let Ok(netlink) = NetlinkSocket::from_fd(fd) {
            // Netlink is a FileLike, not an ax_net Socket, so the flag-aware recv
            // path below is unreachable for it. Honor the recv flags here:
            // MSG_PEEK (do not consume the dump — getifaddrs/dnsmasq peek-then-
            // read to size their buffer), MSG_TRUNC (full datagram length),
            // MSG_DONTWAIT (non-blocking).
            let (recv, truncated) = netlink.recv(
                &mut dst,
                flags & MSG_PEEK != 0,
                flags & MSG_TRUNC != 0,
                flags & MSG_DONTWAIT != 0,
            )?;
            // Surface MSG_TRUNC in the returned `msg_flags` when the datagram
            // did not fit (Linux sets it; getifaddrs sizes its buffer from it).
            *truncated_out = truncated;
            let name_len = if let Some(name) = name {
                let mut len = name.capacity.read()?;
                super::addr::write_netlink_addr(&netlink.kernel_addr(), name.addr, &mut len)?;
                Some(len)
            } else {
                None
            };
            if let Some(builder) = cmsg_builder.take() {
                builder.finish();
            }
            return Ok(RecvOutcome {
                received: recv as isize,
                name_len,
            });
        }

        get_file_like(fd)?;
        return Err(StarryError::NotASocket);
    };
    let mut recv_flags = RecvFlags::empty();
    if flags & MSG_PEEK != 0 {
        recv_flags |= RecvFlags::PEEK;
    }
    if flags & MSG_TRUNC != 0 {
        recv_flags |= RecvFlags::TRUNCATE;
    }
    if flags & MSG_DONTWAIT != 0 {
        recv_flags |= RecvFlags::DONTWAIT;
    }
    if flags & MSG_OOB != 0 {
        recv_flags |= RecvFlags::OOB;
    }
    // Received SCM_RIGHTS fds get O_CLOEXEC when the caller passes
    // MSG_CMSG_CLOEXEC (recvmsg(2)); Linux net/core/scm.c scm_detach_fds.
    let cmsg_cloexec = flags & MSG_CMSG_CLOEXEC != 0;

    let mut cmsg = Vec::new();

    let mut remote_addr = name.map(|_| SocketAddrEx::Ip((Ipv4Addr::UNSPECIFIED, 0).into()));
    let recv = socket.recv(
        &mut dst,
        RecvOptions {
            from: remote_addr.as_mut(),
            flags: recv_flags,
            cmsg: Some(&mut cmsg),
            truncated: Some(truncated_out),
        },
    )?;

    let name_len = if let (Some(remote_addr), Some(name)) = (remote_addr, name) {
        let mut len = name.capacity.read()?;
        socket_addr_ex_for_user_name(socket.ip_domain(), remote_addr)
            .write_to_user(name.addr, &mut len)?;
        Some(len)
    } else {
        None
    };

    if cmsg_builder.is_none() && !cmsg.is_empty() {
        *control_truncated_out = true;
    }
    if let Some(mut builder) = cmsg_builder {
        for cmsg in cmsg {
            let pushed = match cmsg.into_any().downcast::<CMsg>() {
                Ok(cmsg) => match *cmsg {
                    CMsg::Rights { fds } => {
                        let total = fds.len();
                        let installed = builder.push_rights(fds, cmsg_cloexec)?;
                        if installed < total {
                            *control_truncated_out = true;
                        }
                        installed != 0
                    }
                },
                Err(cmsg) => match cmsg.downcast::<IpCmsg>() {
                    Ok(cmsg) => match *cmsg {
                        IpCmsg::Ipv4Ttl(ttl) => {
                            builder.push_sized(PROTO_IP, IP_TTL, size_of::<i32>(), |data| {
                                data.copy_from_slice(&i32::from(ttl).to_ne_bytes());
                                Ok(size_of::<i32>())
                            })?
                        }
                        IpCmsg::Ipv4Tos(tos) => {
                            builder.push_sized(PROTO_IP, IP_TOS, 1, |data| {
                                data[0] = tos;
                                Ok(1)
                            })?
                        }
                        IpCmsg::Ipv6TrafficClass(tclass) => builder.push_sized(
                            IPPROTO_IPV6 as u32,
                            IPV6_TCLASS,
                            size_of::<i32>(),
                            |data| {
                                data.copy_from_slice(&i32::from(tclass).to_ne_bytes());
                                Ok(size_of::<i32>())
                            },
                        )?,
                    },
                    Err(cmsg) => match cmsg.downcast::<SocketCmsg>() {
                        Ok(cmsg) => match *cmsg {
                            SocketCmsg::Credentials(credentials) => {
                                let credentials = Socket::project_unix_credentials(&credentials);
                                builder.push_sized(
                                    SOL_SOCKET,
                                    SCM_CREDENTIALS,
                                    size_of::<ucred>(),
                                    |data| {
                                        let credentials = ucred {
                                            pid: credentials.pid as _,
                                            uid: credentials.uid,
                                            gid: credentials.gid,
                                        };
                                        // SAFETY: `credentials` lives through the
                                        // copy, and `ucred` is a plain C ABI record.
                                        data.copy_from_slice(unsafe {
                                            core::slice::from_raw_parts(
                                                (&credentials as *const ucred).cast::<u8>(),
                                                size_of::<ucred>(),
                                            )
                                        });
                                        Ok(size_of::<ucred>())
                                    },
                                )?
                            }
                            SocketCmsg::Timestamp(timestamp) => builder.push_sized(
                                SOL_SOCKET,
                                SCM_TIMESTAMP,
                                size_of::<timeval>(),
                                |data| {
                                    let timestamp = timeval::from_time_value(timestamp);
                                    // SAFETY: `timestamp` lives through the
                                    // copy, and `timeval` is a plain C ABI
                                    // record with no padding requirements for
                                    // reading its initialized byte layout.
                                    data.copy_from_slice(unsafe {
                                        core::slice::from_raw_parts(
                                            (&timestamp as *const timeval).cast::<u8>(),
                                            size_of::<timeval>(),
                                        )
                                    });
                                    Ok(size_of::<timeval>())
                                },
                            )?,
                        },
                        Err(_) => {
                            warn!("received unexpected cmsg");
                            continue;
                        }
                    },
                },
            };
            if !pushed {
                *control_truncated_out = true;
                break;
            }
        }
        builder.finish();
    }

    debug!("sys_recv => fd: {fd}, recv: {recv}");
    Ok(RecvOutcome {
        received: recv as isize,
        name_len,
    })
}

pub fn sys_recvfrom(
    fd: i32,
    buf: *mut u8,
    len: usize,
    flags: u32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> StarryResult<isize> {
    let name = if addr.is_null() {
        None
    } else {
        Some(RecvName {
            addr,
            capacity: RecvNameCapacity::User(addrlen),
        })
    };
    let outcome = recv_impl(
        fd,
        VmBytesMut::new(buf, len),
        flags,
        name,
        None,
        &mut false,
        &mut false,
    )?;
    if let Some(name_len) = outcome.name_len {
        addrlen.vm_write(name_len)?;
    }
    Ok(outcome.received)
}

pub fn sys_recvmsg(fd: i32, msg: *mut msghdr, flags: u32) -> StarryResult<isize> {
    // SAFETY: Linux `msghdr` contains only integers and raw pointers.
    let mut header = unsafe { msg.vm_read_any()? };
    let mut truncated = false;
    let mut control_truncated = false;
    let name = (!header.msg_name.is_null()).then_some(RecvName {
        addr: header.msg_name.cast(),
        capacity: RecvNameCapacity::Header(header.msg_namelen as socklen_t),
    });
    let outcome = recv_impl(
        fd,
        IoVectorBuf::new(header.msg_iov as *mut IoVec, header.msg_iovlen)?.into_io(),
        flags,
        name,
        (!header.msg_control.is_null()).then(|| {
            CMsgBuilder::new(
                header.msg_control as *mut cmsghdr,
                &mut header.msg_controllen,
            )
        }),
        &mut truncated,
        &mut control_truncated,
    )?;
    if let Some(name_len) = outcome.name_len {
        header.msg_namelen = name_len as _;
    }
    // Linux: on success, set msg.msg_flags to indicate truncation etc.
    let mut mf = 0;
    if truncated {
        mf |= MSG_TRUNC;
    }
    if control_truncated {
        mf |= MSG_CTRUNC;
    }
    header.msg_flags = mf;
    write_recvmsg_outputs(msg, &header)?;
    Ok(outcome.received)
}

/// Copies only Linux's value-result fields. Input pointers may reside in a
/// read-only page and must not be overwritten by a whole-structure copyout.
fn write_recvmsg_outputs(pointer: *mut msghdr, header: &msghdr) -> StarryResult<()> {
    if !header.msg_name.is_null() {
        pointer
            .cast::<u8>()
            .wrapping_add(core::mem::offset_of!(msghdr, msg_namelen))
            .cast::<i32>()
            .vm_write(header.msg_namelen)?;
    }
    pointer
        .cast::<u8>()
        .wrapping_add(core::mem::offset_of!(msghdr, msg_flags))
        .cast::<u32>()
        .vm_write(header.msg_flags)?;
    pointer
        .cast::<u8>()
        .wrapping_add(core::mem::offset_of!(msghdr, msg_controllen))
        .cast::<usize>()
        .vm_write(header.msg_controllen)?;
    Ok(())
}

fn write_mmsg_len(pointer: *mut mmsghdr, len: u32) -> StarryResult<()> {
    pointer
        .cast::<u8>()
        .wrapping_add(core::mem::offset_of!(mmsghdr, msg_len))
        .cast::<u32>()
        .vm_write(len)?;
    Ok(())
}

/// Send multiple datagrams in one syscall.
pub fn sys_sendmmsg(fd: i32, msgvec: *mut mmsghdr, vlen: u32, flags: u32) -> StarryResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    // Linux clamps vlen to UIO_MAXIOV and proceeds (net/socket.c:2796); it
    // never rejects an over-cap batch with EINVAL.
    let vlen = vlen.min(MMSG_MAX_VLEN);

    let mut sent = 0;
    for index in 0..vlen as usize {
        let slot = msgvec.wrapping_add(index);
        // Include import and copyout failures in the per-message result:
        // once a prefix completed, Linux returns its count over a later error.
        let result = (|| -> StarryResult<()> {
            // SAFETY: Linux mmsghdr contains only integers and raw pointers.
            let msg = unsafe { slot.vm_read_any()? };
            let cmsg =
                parse_send_cmsgs(msg.msg_hdr.msg_control as usize, msg.msg_hdr.msg_controllen)?;
            let sent = send_impl(
                fd,
                IoVectorBuf::new(msg.msg_hdr.msg_iov as *const IoVec, msg.msg_hdr.msg_iovlen)?
                    .into_io(),
                flags,
                msg.msg_hdr.msg_name.cast(),
                msg.msg_hdr.msg_namelen as socklen_t,
                cmsg,
            )?;
            write_mmsg_len(slot, sent as u32)
        })();
        match result {
            Ok(()) => sent += 1,
            Err(error) if sent == 0 => return Err(error),
            Err(_) => break,
        }
    }
    Ok(sent)
}

/// Receive multiple datagrams in one syscall.
pub fn sys_recvmmsg(
    fd: i32,
    msgvec: *mut mmsghdr,
    vlen: u32,
    flags: u32,
    timeout: *const timespec,
) -> StarryResult<isize> {
    if vlen == 0 {
        return Ok(0);
    }
    // Linux do_recvmmsg does not cap vlen; StarryOS bounds the batch to
    // UIO_MAXIOV so the fallible VM copy imports a bounded user array. Clamp
    // rather than reject with EINVAL so an over-cap batch still makes
    // progress, matching sendmmsg's UIO_MAXIOV clamp (net/socket.c:2796).
    let vlen = vlen.min(MMSG_MAX_VLEN);

    let timeout = parse_recvmmsg_timeout(timeout)?;
    // TODO: deadline is only checked between recv_impl calls. If a single
    // recv_impl blocks waiting for data (socket has nothing to read), the
    // deadline cannot interrupt it. Needs a non-blocking recv path or
    // SO_RCVTIMEO support at the socket layer to fix.
    let deadline = timeout.map(|t| monotonic_time() + t);
    let _socket = Socket::from_fd(fd)?;
    let mut received = 0;
    let mut flags = flags;
    for index in 0..vlen as usize {
        if let Some(deadline) = deadline
            && monotonic_time() >= deadline
        {
            if received == 0 {
                return Err(StarryError::WouldBlock);
            }
            break;
        }

        let slot = msgvec.wrapping_add(index);
        let result = (|| -> StarryResult<()> {
            // SAFETY: Linux `mmsghdr` contains only integer fields and raw
            // pointers through its nested `msghdr`.
            let mut msg = unsafe { slot.vm_read_any()? };
            let name = (!msg.msg_hdr.msg_name.is_null()).then_some(RecvName {
                addr: msg.msg_hdr.msg_name.cast(),
                capacity: RecvNameCapacity::Header(msg.msg_hdr.msg_namelen as socklen_t),
            });
            let mut truncated = false;
            let mut control_truncated = false;
            let outcome = recv_impl(
                fd,
                IoVectorBuf::new(msg.msg_hdr.msg_iov as *mut IoVec, msg.msg_hdr.msg_iovlen)?
                    .into_io(),
                flags,
                name,
                (!msg.msg_hdr.msg_control.is_null()).then(|| {
                    CMsgBuilder::new(
                        msg.msg_hdr.msg_control as *mut cmsghdr,
                        &mut msg.msg_hdr.msg_controllen,
                    )
                }),
                &mut truncated,
                &mut control_truncated,
            )?;

            if let Some(name_len) = outcome.name_len {
                msg.msg_hdr.msg_namelen = name_len as _;
            }
            msg.msg_hdr.msg_flags =
                u32::from(truncated) * MSG_TRUNC + u32::from(control_truncated) * MSG_CTRUNC;
            let header_pointer = slot
                .cast::<u8>()
                .wrapping_add(core::mem::offset_of!(mmsghdr, msg_hdr))
                .cast::<msghdr>();
            write_recvmsg_outputs(header_pointer, &msg.msg_hdr)?;
            write_mmsg_len(slot, outcome.received as u32)
        })();
        match result {
            Ok(()) => {
                received += 1;
                // MSG_WAITFORONE: once a datagram is received, remaining
                // recvs must not block (Linux do_recvmmsg net/socket.c:3055
                // sets MSG_DONTWAIT after the first packet). Without this a
                // vlen>1 recvmmsg on a socket with fewer datagrams blocks
                // forever on the next recv.
                if flags & MSG_WAITFORONE != 0 {
                    flags |= MSG_DONTWAIT;
                }
            }
            Err(e) => {
                if received == 0 {
                    return Err(e);
                }
                break;
            }
        }
    }

    Ok(received)
}

#[cfg(all(test, not(axtest)))]
fn net_io_constants_hold_for_test() -> bool {
    const {
        assert!(MMSG_MAX_VLEN == 1024);
        assert!(PROTO_IP == 0);
    }

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn net_io_constants_hold() {
        assert!(super::net_io_constants_hold_for_test());
    }
}
