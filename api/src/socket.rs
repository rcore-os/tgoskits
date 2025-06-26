//! Wrapper for [`sockaddr`]. Using trait to convert between [`SocketAddr`] and [`sockaddr`] types.

use crate::ptr::{UserConstPtr, UserPtr};
use axerrno::{LinuxError, LinuxResult};
use core::{
    mem::{MaybeUninit, size_of},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};
use linux_raw_sys::net::{
    __kernel_sa_family_t, AF_INET, AF_INET6, in_addr, in6_addr, sockaddr, sockaddr_in,
    sockaddr_in6, socklen_t,
};

/// Trait to extend [`SocketAddr`] and its variants with methods for reading from and writing to user space.
///
pub trait SocketAddrExt: Sized {
    /// This method attempts to interpret the data pointed to by `addr` with the
    /// given `addrlen` as a valid socket address of the implementing type.
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> LinuxResult<Self>;

    /// This method serializes the current socket address instance into the
    /// [`sockaddr`] structure pointed to by `addr` in user space.
    fn write_to_user(&self, addr: UserPtr<sockaddr>) -> LinuxResult<socklen_t>;

    /// Gets the address family of the socket address.
    fn family(&self) -> u16;

    /// Gets the encoded length of the socket address.
    fn addr_len(&self) -> socklen_t;
}

/// Copies a socket address from user space into a temporary kernel storage.
///
/// This function reads `addrlen` bytes from the user-space pointer `addr` and
/// copies them into a `MaybeUninit<sockaddr>` in kernel memory.
///
#[inline]
fn copy_sockaddr_from_user(
    addr: UserConstPtr<sockaddr>,
    addrlen: socklen_t,
) -> LinuxResult<MaybeUninit<sockaddr>> {
    let mut storage = MaybeUninit::<sockaddr>::uninit();
    let sock_addr = addr.get_as_ref()?;
    unsafe {
        core::ptr::copy_nonoverlapping(
            sock_addr as *const sockaddr as *const u8,
            storage.as_mut_ptr() as *mut u8,
            addrlen as usize,
        )
    };
    Ok(storage)
}

impl SocketAddrExt for SocketAddr {
    /// Reads a [`SocketAddr`] from user space.
    ///
    /// This implementation first performs basic length validation. Then, it copies
    /// the raw [`sockaddr`] data from user space into a temporary kernel buffer.
    /// Based on the address family ([`AF_INET`] or [`AF_INET6`]) extracted from the
    /// copied data, it delegates the actual parsing to [`SocketAddrV4::read_from_user`]
    /// or [`SocketAddrV6::read_from_user`].
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> LinuxResult<Self> {
        if size_of::<__kernel_sa_family_t>() > addrlen as usize
            || addrlen as usize > size_of::<sockaddr>()
        {
            return Err(LinuxError::EINVAL);
        }
        let src_addr = addr.get_as_ref()?;
        let family = unsafe {
            src_addr
                .__storage
                .__bindgen_anon_1
                .__bindgen_anon_1
                .ss_family as u32
        };
        match family {
            AF_INET => SocketAddrV4::read_from_user(addr, addrlen).map(SocketAddr::V4),
            AF_INET6 => SocketAddrV6::read_from_user(addr, addrlen).map(SocketAddr::V6),
            _ => Err(LinuxError::EAFNOSUPPORT),
        }
    }

    /// Writes the [`SocketAddr`] to user space.
    ///
    /// This implementation checks for a null user-space pointer. Then, it delegates
    /// the actual writing to the specific [`SocketAddrV4`] or [`SocketAddrV6`]
    /// `write_to_user` implementation based on the variant of `self`.
    fn write_to_user(&self, addr: UserPtr<sockaddr>) -> LinuxResult<socklen_t> {
        if addr.is_null() {
            return Err(LinuxError::EINVAL);
        }

        match self {
            SocketAddr::V4(v4) => v4.write_to_user(addr),
            SocketAddr::V6(v6) => v6.write_to_user(addr),
        }
    }

    /// Gets the address family of the [`SocketAddr`].
    ///
    /// Returns `AF_INET` for IPv4 addresses or `AF_INET6` for IPv6 addresses.
    fn family(&self) -> u16 {
        match self {
            SocketAddr::V4(v4) => v4.family(),
            SocketAddr::V6(v6) => v6.family(),
        }
    }

    /// Gets the encoded length of the [`SocketAddr`] instance.
    ///
    /// Returns the size in bytes that this [`SocketAddr`] would occupy when
    /// encoded as a [`sockaddr_in`] (for IPv4) or [`sockaddr_in6`] (for IPv6) structure.
    fn addr_len(&self) -> socklen_t {
        match self {
            SocketAddr::V4(v4) => v4.addr_len(),
            SocketAddr::V6(v6) => v6.addr_len(),
        }
    }
}

impl SocketAddrExt for SocketAddrV4 {
    /// Reads an [`SocketAddrV4`] from user space.
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> LinuxResult<Self> {
        if addrlen < size_of::<sockaddr_in>() as socklen_t {
            return Err(LinuxError::EINVAL);
        }
        let storage = copy_sockaddr_from_user(addr, addrlen)?;
        let addr_in = unsafe { &*(storage.as_ptr() as *const sockaddr_in) };
        if addr_in.sin_family as u32 != AF_INET {
            return Err(LinuxError::EAFNOSUPPORT);
        }

        Ok(SocketAddrV4::new(
            Ipv4Addr::from_bits(u32::from_be(addr_in.sin_addr.s_addr)),
            u16::from_be(addr_in.sin_port),
        ))
    }

    /// Writes the `SocketAddrV4` to user space.
    fn write_to_user(&self, addr: UserPtr<sockaddr>) -> LinuxResult<socklen_t> {
        if addr.is_null() {
            return Err(LinuxError::EINVAL);
        }
        let dst_addr = addr.get_as_mut()?;
        let len = size_of::<sockaddr_in>() as socklen_t;
        let sockin_addr = sockaddr_in {
            sin_family: AF_INET as _,
            sin_port: self.port().to_be(),
            sin_addr: in_addr {
                s_addr: u32::from_ne_bytes(self.ip().octets()),
            },
            __pad: [0_u8; 8],
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                &sockin_addr as *const sockaddr_in as *const u8,
                dst_addr as *mut sockaddr as *mut u8,
                len as usize,
            )
        };

        Ok(len)
    }

    /// Gets the address family for [`SocketAddrV4`].
    fn family(&self) -> u16 {
        AF_INET as u16
    }

    /// Gets the encoded length of [`SocketAddrV4`].
    fn addr_len(&self) -> socklen_t {
        size_of::<sockaddr_in>() as socklen_t
    }
}

impl SocketAddrExt for SocketAddrV6 {
    /// Reads an [`SocketAddrV6`] from user space.
    fn read_from_user(addr: UserConstPtr<sockaddr>, addrlen: socklen_t) -> LinuxResult<Self> {
        if addrlen < size_of::<sockaddr_in6>() as socklen_t {
            return Err(LinuxError::EINVAL);
        }
        let storage = copy_sockaddr_from_user(addr, addrlen)?;
        let addr_in6 = unsafe { &*(storage.as_ptr() as *const sockaddr_in6) };
        if addr_in6.sin6_family as u32 != AF_INET6 {
            return Err(LinuxError::EAFNOSUPPORT);
        }

        Ok(SocketAddrV6::new(
            Ipv6Addr::from(unsafe { addr_in6.sin6_addr.in6_u.u6_addr8 }),
            u16::from_be(addr_in6.sin6_port),
            u32::from_be(addr_in6.sin6_flowinfo),
            addr_in6.sin6_scope_id,
        ))
    }
    /// Writes the `SocketAddrV6` to user space.
    fn write_to_user(&self, addr: UserPtr<sockaddr>) -> LinuxResult<socklen_t> {
        if addr.is_null() {
            return Err(LinuxError::EINVAL);
        }
        let dst_addr = addr.get_as_mut()?;
        let len = size_of::<sockaddr_in6>() as socklen_t;
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

        unsafe {
            core::ptr::copy_nonoverlapping(
                &sockin_addr as *const sockaddr_in6 as *const u8,
                dst_addr as *mut sockaddr as *mut u8,
                len as usize,
            )
        };

        Ok(len)
    }

    /// Gets the address family for [`SocketAddrV6`].
    fn family(&self) -> u16 {
        AF_INET6 as u16
    }

    /// Gets the encoded length of [`SocketAddrV6`].
    fn addr_len(&self) -> socklen_t {
        size_of::<sockaddr_in6>() as socklen_t
    }
}
