use alloc::sync::Arc;
use core::ffi::c_char;

use ax_kspin::SpinNoIrq;

mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

/// The initial root UTS namespace, shared by all processes until
/// they call `unshare(CLONE_NEWUTS)`.
pub static ROOT_UTS_NS: spin::LazyLock<Arc<SpinNoIrq<UtNamespace>>> =
    spin::LazyLock::new(|| Arc::new(SpinNoIrq::new(UtNamespace::new_root())));

const fn pad_str(info: &str) -> [c_char; 65] {
    let mut data: [c_char; 65] = [0; 65];
    unsafe {
        core::ptr::copy_nonoverlapping(info.as_ptr().cast(), data.as_mut_ptr(), info.len());
    }
    data
}

/// Per-process UTS namespace, containing the hostname and domain name
/// visible to `uname(2)`.  When a process calls `unshare(CLONE_NEWUTS)` or
/// `clone(CLONE_NEWUTS)`, it receives a fresh copy of the parent namespace
/// so that subsequent `sethostname(2)` / `setdomainname(2)` do not affect
/// the original namespace.
pub struct UtNamespace {
    pub nodename: [c_char; 65],
    pub domainname: [c_char; 65],
}

impl UtNamespace {
    /// Create the initial root UTS namespace with default values.
    pub fn new_root() -> Self {
        Self {
            nodename: pad_str("starry"),
            domainname: pad_str("https://github.com/Starry-OS/StarryOS"),
        }
    }

    /// Clone the namespace (shallow copy of nodename/domainname).
    pub fn clone_ns(&self) -> Self {
        Self {
            nodename: self.nodename,
            domainname: self.domainname,
        }
    }
}

/// Build a `new_utsname` from a UTS namespace.
/// The `sysname`, `release`, `version`, and `machine` fields are
/// system-wide constants; only `nodename` and `domainname` are
/// per-namespace.
pub fn build_utsname(ns: &UtNamespace) -> linux_raw_sys::system::new_utsname {
    linux_raw_sys::system::new_utsname {
        sysname: pad_str("Linux"),
        nodename: ns.nodename,
        release: pad_str("10.0.0"),
        version: pad_str("10.0.0"),
        machine: pad_str(build_info::ARCH),
        domainname: ns.domainname,
    }
}
