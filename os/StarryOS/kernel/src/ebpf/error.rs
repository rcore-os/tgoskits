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

#[cfg(axtest)]
pub(crate) fn bpf_error_more_variants_and_edge_cases_hold_for_test() -> bool {
    // Test more BpfError variants mapping through bpf_err_to_ax
    let e2big: AxError = bpf_err_to_ax(kbpf_basic::BpfError::E2BIG);
    assert!(e2big == AxError::from(ax_errno::LinuxError::E2BIG));

    let enoent: AxError = bpf_err_to_ax(kbpf_basic::BpfError::ENOENT);
    assert!(enoent == AxError::from(ax_errno::LinuxError::ENOENT));

    let einval: AxError = bpf_err_to_ax(kbpf_basic::BpfError::EINVAL);
    assert!(einval == AxError::from(ax_errno::LinuxError::EINVAL));

    // Test BpfResultExt with different types
    let ok_u8: kbpf_basic::BpfResult<u8> = Ok(255u8);
    assert!(ok_u8.into_ax_result().unwrap() == 255);

    let ok_i64: kbpf_basic::BpfResult<i64> = Ok(-1i64);
    assert!(ok_i64.into_ax_result().unwrap() == -1);

    let ok_unit: kbpf_basic::BpfResult<()> = Ok(());
    assert!(ok_unit.into_ax_result().is_ok());

    // More error variants
    let eacces: AxError = bpf_err_to_ax(kbpf_basic::BpfError::EACCES);
    assert!(eacces == AxError::from(ax_errno::LinuxError::EACCES));

    let efault: AxError = bpf_err_to_ax(kbpf_basic::BpfError::EFAULT);
    assert!(efault == AxError::from(ax_errno::LinuxError::EFAULT));

    let enomem: AxError = bpf_err_to_ax(kbpf_basic::BpfError::ENOMEM);
    assert!(enomem == AxError::from(ax_errno::LinuxError::ENOMEM));

    let nosys: AxError = bpf_err_to_ax(kbpf_basic::BpfError::ENOSYS);
    assert!(nosys == AxError::from(ax_errno::LinuxError::ENOSYS));

    true
}
