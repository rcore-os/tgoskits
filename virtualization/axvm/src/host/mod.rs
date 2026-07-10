//! Internal host boundary used by the AxVM runtime.

pub(crate) mod arceos;
pub(crate) mod paging;
pub(crate) mod task;
pub(crate) mod traits;

pub(crate) fn default_host() -> &'static arceos::ArceOsHost {
    arceos::arceos_host()
}

pub(crate) use paging::PagingHandler;
pub(crate) use traits::{HostCpu, HostMemory, HostPlatform, HostTime};
