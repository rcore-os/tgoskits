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

#[cfg(axtest)]
pub(crate) fn bpf_error_adapter_rules_hold_for_test() -> bool {
    // bpf_err_to_ax: known LinuxError codes map through; unknown fallback to EINVAL.
    let r1: AxError = bpf_err_to_ax(kbpf_basic::BpfError::ENOMEM);
    let r1_matches = r1 == AxError::from(ax_errno::LinuxError::ENOMEM);

    // BpfError::EINVAL maps to LinuxError::EINVAL (the fallback case).
    let r2: AxError = bpf_err_to_ax(kbpf_basic::BpfError::EINVAL);
    let r2_matches = r2 == AxError::from(ax_errno::LinuxError::EINVAL);

    // BpfResultExt::into_ax_result: Ok passes through, Err maps via bpf_err_to_ax.
    let ok_val: kbpf_basic::BpfResult<u32> = Ok(42u32);
    let ok_mapped = ok_val.into_ax_result();
    let ok_ok = ok_mapped.is_ok() && ok_mapped.unwrap() == 42;

    let err_val: kbpf_basic::BpfResult<u32> = Err(kbpf_basic::BpfError::EPERM);
    let err_mapped = err_val.into_ax_result();
    let err_is_perm = err_mapped.is_err()
        && err_mapped.unwrap_err() == AxError::from(ax_errno::LinuxError::EPERM);

    r1_matches && r2_matches && ok_ok && err_is_perm
}
