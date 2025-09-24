//! Wrapper for [`sockaddr`]. Using trait to convert between [`SocketAddr`] and
//! [`sockaddr`] types.

use alloc::vec::Vec;
use core::{
    mem::size_of,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};

use axerrno::{AxError, AxResult, LinuxError};
use axnet::{SocketAddrEx, unix::UnixSocketAddr};
use linux_raw_sys::net::{
    __kernel_sa_family_t, AF_INET, AF_INET6, AF_UNIX, in_addr, in6_addr, sockaddr, sockaddr_in,
    sockaddr_in6, socklen_t,
};

use crate::mm::{UserConstPtr, UserPtr};

/// Trait to extend [`SocketAddr`] and its variants with methods for reading
/// from and writing to user space.
pub trait SocketAddrExt: Sized {
    /// This method attempts to interpret the data pointed to by `addr` with the
    /// given `addrlen` as a valid socket address of the implementing type.
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self>;

    /// This method serializes the current socket address instance into the
    /// [`sockaddr`] structure pointed to by `addr` in user space.
    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()>;

    /// Gets the address family of the socket address.
    fn family(&self) -> u16;
}

fn read_family(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<u16> {
    if size_of::<__kernel_sa_family_t>() > addrlen as usize {
        return Err(AxError::InvalidInput);
    }
    let family = *addr.cast::<__kernel_sa_family_t>().get_as_ref()?;
    Ok(family)
}
unsafe fn cast_to_slice<T>(value: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}
fn fill_addr(addr: UserPtr<sockaddr>, addrlen: &mut socklen_t, data: &[u8]) -> AxResult<()> {
    let len = (*addrlen as usize).min(data.len());
    addr.cast::<u8>()
        .get_as_mut_slice(len)?
        .copy_from_slice(&data[..len]);
    *addrlen = data.len() as _;
    Ok(())
}

impl SocketAddrExt for SocketAddr {
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self> {
        match read_family(addr, addrlen)? as u32 {
            AF_INET => SocketAddrV4::read_from_user(addr, addrlen).map(Self::V4),
            AF_INET6 => SocketAddrV6::read_from_user(addr, addrlen).map(Self::V6),
            _ => Err(AxError::Other(LinuxError::EAFNOSUPPORT)),
        }
    }

    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()> {
        match self {
            SocketAddr::V4(v4) => v4.write_to_user(addr, addrlen),
            SocketAddr::V6(v6) => v6.write_to_user(addr, addrlen),
        }
    }

    fn family(&self) -> u16 {
        match self {
            SocketAddr::V4(v4) => v4.family(),
            SocketAddr::V6(v6) => v6.family(),
        }
    }
}

impl SocketAddrExt for SocketAddrV4 {
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self> {
        if addrlen != size_of::<sockaddr_in>() as socklen_t {
            return Err(AxError::InvalidInput);
        }
        let addr_in = addr.cast::<sockaddr_in>().get_as_ref()?;
        if addr_in.sin_family as u32 != AF_INET {
            return Err(AxError::Other(LinuxError::EAFNOSUPPORT));
        }

        Ok(SocketAddrV4::new(
            Ipv4Addr::from_bits(u32::from_be(addr_in.sin_addr.s_addr)),
            u16::from_be(addr_in.sin_port),
        ))
    }

    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()> {
        let sockin_addr = sockaddr_in {
            sin_family: AF_INET as _,
            sin_port: self.port().to_be(),
            sin_addr: in_addr {
                s_addr: u32::from_ne_bytes(self.ip().octets()),
            },
            __pad: [0_u8; 8],
        };
        fill_addr(addr, addrlen, unsafe { cast_to_slice(&sockin_addr) })
    }

    fn family(&self) -> u16 {
        AF_INET as u16
    }
}

impl SocketAddrExt for SocketAddrV6 {
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self> {
        if addrlen != size_of::<sockaddr_in6>() as socklen_t {
            return Err(AxError::InvalidInput);
        }
        let addr_in6 = addr.cast::<sockaddr_in6>().get_as_ref()?;
        if addr_in6.sin6_family as u32 != AF_INET6 {
            return Err(AxError::Other(LinuxError::EAFNOSUPPORT));
        }

        Ok(SocketAddrV6::new(
            Ipv6Addr::from(unsafe { addr_in6.sin6_addr.in6_u.u6_addr8 }),
            u16::from_be(addr_in6.sin6_port),
            u32::from_be(addr_in6.sin6_flowinfo),
            addr_in6.sin6_scope_id,
        ))
    }

    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()> {
        let sockin_addr = sockaddr_in6 {
            sin6_family: AF_INET6 as _,
            sin6_port: self.port().to_be(),
            sin6_flowinfo: self.flowinfo().to_be(),
            sin6_addr: in6_addr {
                in6_u: linux_raw_sys::net::in6_addr__bindgen_ty_1 {
                    u6_addr8: self.ip().octets(),
                },
            },
            sin6_scope_id: self.scope_id(),
        };
        fill_addr(addr, addrlen, unsafe { cast_to_slice(&sockin_addr) })
    }

    fn family(&self) -> u16 {
        AF_INET6 as u16
    }
}

impl SocketAddrExt for UnixSocketAddr {
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self> {
        if read_family(addr, addrlen)? as u32 != AF_UNIX {
            return Err(AxError::Other(LinuxError::EAFNOSUPPORT));
        }
        let offset = size_of::<__kernel_sa_family_t>();
        let ptr = UserConstPtr::<u8>::from(addr.address().as_usize() + offset);
        let data = ptr.get_as_slice(addrlen as usize - offset)?;
        Ok(if data.is_empty() {
            Self::Unnamed
        } else if data[0] == 0 {
            Self::Abstract(data[1..].into())
        } else {
            let end = data.iter().position(|&c| c == 0).unwrap_or(data.len());
            Self::Path(
                str::from_utf8(&data[..end])
                    .map_err(|_| AxError::InvalidInput)?
                    .into(),
            )
        })
    }

    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()> {
        let data_len = match self {
            UnixSocketAddr::Unnamed => 0,
            UnixSocketAddr::Abstract(name) => name.len() + 1,
            UnixSocketAddr::Path(path) => 1 + path.len(),
        };
        let mut buf = Vec::with_capacity(size_of::<__kernel_sa_family_t>() + data_len);
        buf.extend_from_slice(&AF_UNIX.to_ne_bytes());
        match self {
            UnixSocketAddr::Unnamed => {}
            UnixSocketAddr::Abstract(name) => {
                buf.push(0);
                buf.extend_from_slice(name);
            }
            UnixSocketAddr::Path(path) => {
                buf.extend_from_slice(path.as_bytes());
                buf.push(0);
            }
        }

        fill_addr(addr, addrlen, &buf)
    }

    fn family(&self) -> u16 {
        AF_UNIX as u16
    }
}

impl SocketAddrExt for SocketAddrEx {
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> AxResult<Self> {
        match read_family(addr, addrlen)? as u32 {
            AF_INET | AF_INET6 => SocketAddr::read_from_user(addr, addrlen).map(Self::Ip),
            AF_UNIX => UnixSocketAddr::read_from_user(addr, addrlen).map(Self::Unix),
            _ => Err(AxError::Other(LinuxError::EAFNOSUPPORT)),
        }
    }

    fn write_to_user(&self, addr: UserPtr<sockaddr>, addrlen: &mut socklen_t) -> AxResult<()> {
        match self {
            SocketAddrEx::Ip(ip_addr) => ip_addr.write_to_user(addr, addrlen),
            SocketAddrEx::Unix(unix_addr) => unix_addr.write_to_user(addr, addrlen),
        }
    }

    fn family(&self) -> u16 {
        AF_INET as u16
    }
}
