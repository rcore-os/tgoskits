//! Wrapper for `sockaddr`. Adapted from [`rustix::net::SocketAddrAny`].
//!
//! [`rustix::net::SocketAddrAny`]: https://docs.rs/rustix/latest/rustix/net/struct.SocketAddrAny.html

use core::{
    mem::MaybeUninit,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
};

use axerrno::{LinuxError, LinuxResult};
use linux_raw_sys::net::{
    __kernel_sa_family_t, AF_INET, AF_INET6, in_addr, in6_addr, sockaddr, sockaddr_in,
    sockaddr_in6, socklen_t,
};

/// A type that can hold any kind of socket address, as a safe abstraction for
/// `sockaddr`.
///
/// Socket addresses can be converted to `SocketAddrAny` via the [`From`] and
/// [`Into`] traits. `SocketAddrAny` can be converted back to a specific socket
/// address type with [`TryFrom`] and [`TryInto`]. These implementations return
/// [`LinuxError::EAFNOSUPPORT`] if the address family does not match the requested
/// type.
#[derive(Clone)]
pub struct SockAddr {
    // Invariants:
    //  - `len` is at least `size_of::<__kernel_sa_family_t>()`
    //  - `len` is at most `size_of::<sockaddr>()`
    //  - The first `len` bytes of `storage` are initialized.
    len: socklen_t,
    storage: MaybeUninit<sockaddr>,
}

// SAFETY: Bindgen adds a union with a raw pointer for alignment but it's never
// used. `sockaddr` is just a bunch of bytes and it doesn't hold pointers.
unsafe impl Send for SockAddr {}

// SAFETY: Same as with `Send`.
unsafe impl Sync for SockAddr {}

impl SockAddr {
    /// Creates a socket address from reading from `ptr`, which points at `len`
    /// initialized bytes.
    ///
    /// Returns [`LinuxError::EINVAL`] if `len` is smaller than `__kernel_sa_family_t` or larger than
    /// `sockaddr`.
    ///
    /// # Safety
    ///
    ///  - `ptr` must be a pointer to memory containing a valid socket address.
    ///  - `len` bytes must be initialized.
    pub unsafe fn read(ptr: *const sockaddr, len: socklen_t) -> LinuxResult<Self> {
        if size_of::<__kernel_sa_family_t>() < len as usize || len as usize > size_of::<sockaddr>()
        {
            return Err(LinuxError::EINVAL);
        }
        let mut storage = MaybeUninit::<sockaddr>::uninit();
        unsafe {
            core::ptr::copy_nonoverlapping(
                ptr as *const u8,
                storage.as_mut_ptr() as *mut u8,
                len as usize,
            )
        };
        Ok(Self { storage, len })
    }

    /// Gets the address family of this socket address.
    #[inline]
    pub fn family(&self) -> u32 {
        // SAFETY: Our invariants maintain that the `sa_family` field is
        // initialized.
        unsafe {
            self.storage
                .assume_init_ref()
                .__storage
                .__bindgen_anon_1
                .__bindgen_anon_1
                .ss_family as u32
        }
    }

    /// Returns the length of the encoded sockaddr.
    #[inline]
    pub fn addr_len(&self) -> socklen_t {
        self.len
    }

    /// Gets the initialized part of the storage as bytes.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.storage.as_ptr().cast(), self.len as usize) }
    }
}

impl From<SocketAddrV4> for SockAddr {
    fn from(v4: SocketAddrV4) -> Self {
        let addr = sockaddr_in {
            sin_family: AF_INET as _,
            sin_port: v4.port().to_be(),
            sin_addr: in_addr {
                s_addr: u32::from_ne_bytes(v4.ip().octets()),
            },
            __pad: [0_u8; 8],
        };
        unsafe {
            Self::read(
                &addr as *const sockaddr_in as *const sockaddr,
                core::mem::size_of::<sockaddr_in>() as socklen_t,
            )
            .unwrap()
        }
    }
}

impl From<SocketAddrV6> for SockAddr {
    fn from(v6: SocketAddrV6) -> Self {
        let addr = sockaddr_in6 {
            sin6_family: AF_INET6 as _,
            sin6_port: v6.port().to_be(),
            sin6_flowinfo: v6.flowinfo().to_be(),
            sin6_addr: in6_addr {
                in6_u: linux_raw_sys::net::in6_addr__bindgen_ty_1 {
                    u6_addr8: v6.ip().octets(),
                },
            },
            sin6_scope_id: v6.scope_id(),
        };
        unsafe {
            Self::read(
                &addr as *const sockaddr_in6 as *const sockaddr,
                core::mem::size_of::<sockaddr_in6>() as socklen_t,
            )
            .unwrap()
        }
    }
}

impl From<SocketAddr> for SockAddr {
    fn from(addr: SocketAddr) -> Self {
        match addr {
            SocketAddr::V4(v4) => v4.into(),
            SocketAddr::V6(v6) => v6.into(),
        }
    }
}

impl TryFrom<SockAddr> for SocketAddrV4 {
    type Error = LinuxError;

    fn try_from(addr: SockAddr) -> LinuxResult<Self> {
        if addr.family() != AF_INET {
            return Err(LinuxError::EAFNOSUPPORT);
        }
        if size_of::<sockaddr_in>() < addr.addr_len() as usize {
            return Err(LinuxError::EINVAL);
        }
        let addr = unsafe { &*(addr.storage.as_ptr() as *const sockaddr_in) };
        Ok(SocketAddrV4::new(
            Ipv4Addr::from_bits(u32::from_be(addr.sin_addr.s_addr)),
            u16::from_be(addr.sin_port),
        ))
    }
}

impl TryFrom<SockAddr> for SocketAddrV6 {
    type Error = LinuxError;

    fn try_from(addr: SockAddr) -> LinuxResult<Self> {
        if addr.family() != AF_INET6 {
            return Err(LinuxError::EAFNOSUPPORT);
        }
        if size_of::<sockaddr_in6>() < addr.addr_len() as usize {
            return Err(LinuxError::EINVAL);
        }
        let addr = unsafe { &*(addr.storage.as_ptr() as *const sockaddr_in6) };
        Ok(SocketAddrV6::new(
            Ipv6Addr::from(unsafe { addr.sin6_addr.in6_u.u6_addr8 }),
            u16::from_be(addr.sin6_port),
            u32::from_be(addr.sin6_flowinfo),
            addr.sin6_scope_id,
        ))
    }
}

impl TryFrom<SockAddr> for SocketAddr {
    type Error = LinuxError;

    fn try_from(addr: SockAddr) -> LinuxResult<Self> {
        match addr.family() {
            AF_INET => Ok(SocketAddr::V4(addr.try_into()?)),
            AF_INET6 => Ok(SocketAddr::V6(addr.try_into()?)),
            _ => Err(LinuxError::EAFNOSUPPORT),
        }
    }
}
