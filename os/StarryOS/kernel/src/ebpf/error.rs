//! Error adapters for the `kbpf-basic` boundary.

use ax_errno::{AxError, LinuxError};

pub(crate) fn bpf_err_to_ax(err: kbpf_basic::BpfError) -> AxError {
    LinuxError::try_from(err.code())
        .map(AxError::from)
        .unwrap_or_else(|_| AxError::from(LinuxError::EINVAL))
}

pub(crate) trait BpfResultExt<T> {
    fn into_ax_result(self) -> ax_errno::AxResult<T>;
}

impl<T> BpfResultExt<T> for kbpf_basic::BpfResult<T> {
    fn into_ax_result(self) -> ax_errno::AxResult<T> {
        self.map_err(bpf_err_to_ax)
    }
}
