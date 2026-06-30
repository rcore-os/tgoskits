//! Internal host boundary used by the AxVM runtime.

pub(crate) mod arceos;
#[cfg(target_arch = "aarch64")]
pub(crate) mod gic;
#[cfg(target_arch = "x86_64")]
pub(crate) mod irq;
pub(crate) mod paging;
pub(crate) mod task;
pub(crate) mod traits;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86_port;

pub(crate) fn default_host() -> &'static arceos::ArceOsHost {
    arceos::arceos_host()
}

#[cfg(target_arch = "x86_64")]
pub(crate) use traits::HostConsole;
pub(crate) use traits::{HostCpu, HostMemory, HostPlatform, HostTime};
