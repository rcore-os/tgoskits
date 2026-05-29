//! Internal host boundary used by the AxVM runtime.

pub(crate) mod arceos;
pub(crate) mod paging;
pub(crate) mod traits;

#[cfg(target_arch = "x86_64")]
pub(crate) use traits::HostConsole;
pub(crate) use traits::{HostCpu, HostMemory, HostPlatform, HostTime};
