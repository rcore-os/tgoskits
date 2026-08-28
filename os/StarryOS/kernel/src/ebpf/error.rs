//! Error adapters for the `kbpf-basic` boundary.

use crate::{Errno, StarryError};

pub(crate) fn bpf_error_to_starry(err: kbpf_basic::BpfError) -> StarryError {
    let errno = Errno::new(err.code());
    if errno.is_valid() {
        errno.into()
    } else {
        Errno::EINVAL.into()
    }
}

pub(crate) trait BpfResultExt<T> {
    fn into_starry_result(self) -> crate::StarryResult<T>;
}

impl<T> BpfResultExt<T> for kbpf_basic::BpfResult<T> {
    fn into_starry_result(self) -> crate::StarryResult<T> {
        self.map_err(bpf_error_to_starry)
    }
}

#[cfg(all(test, not(axtest)))]
fn bpf_error_adapter_rules_hold_for_test() -> bool {
    // Known Errno codes map through; unknown values fall back to EINVAL.
    let r1: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::ENOMEM);
    let r1_matches = r1.linux_errno() == crate::Errno::ENOMEM;

    // BpfError::EINVAL maps to Errno::EINVAL (the fallback case).
    let r2: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::EINVAL);
    let r2_matches = r2.linux_errno() == crate::Errno::EINVAL;

    // BpfResultExt: Ok passes through and Err maps through the adapter.
    let ok_val: kbpf_basic::BpfResult<u32> = Ok(42u32);
    let ok_mapped = ok_val.into_starry_result();
    let ok_ok = ok_mapped.is_ok() && ok_mapped.unwrap() == 42;

    let err_val: kbpf_basic::BpfResult<u32> = Err(kbpf_basic::BpfError::EPERM);
    let err_mapped = err_val.into_starry_result();
    let err_is_perm =
        matches!(err_mapped, Err(error) if error.linux_errno() == crate::Errno::EPERM);

    r1_matches && r2_matches && ok_ok && err_is_perm
}

#[cfg(all(test, not(axtest)))]
fn bpf_error_more_variants_and_edge_cases_hold_for_test() -> bool {
    // Test more BpfError variants mapping through the Starry adapter.
    let e2big: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::E2BIG);
    assert_eq!(e2big.linux_errno(), crate::Errno::E2BIG);

    let enoent: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::ENOENT);
    assert_eq!(enoent.linux_errno(), crate::Errno::ENOENT);

    let einval: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::EINVAL);
    assert_eq!(einval.linux_errno(), crate::Errno::EINVAL);

    // Test BpfResultExt with different types
    let ok_u8: kbpf_basic::BpfResult<u8> = Ok(255u8);
    assert_eq!(ok_u8.into_starry_result().unwrap(), 255);

    let ok_i64: kbpf_basic::BpfResult<i64> = Ok(-1i64);
    assert_eq!(ok_i64.into_starry_result().unwrap(), -1);

    let ok_unit: kbpf_basic::BpfResult<()> = Ok(());
    assert!(ok_unit.into_starry_result().is_ok());

    // More error variants
    let eacces: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::EACCES);
    assert_eq!(eacces.linux_errno(), crate::Errno::EACCES);

    let efault: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::EFAULT);
    assert_eq!(efault.linux_errno(), crate::Errno::EFAULT);

    let enomem: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::ENOMEM);
    assert_eq!(enomem.linux_errno(), crate::Errno::ENOMEM);

    let nosys: StarryError = bpf_error_to_starry(kbpf_basic::BpfError::ENOSYS);
    assert_eq!(nosys.linux_errno(), crate::Errno::ENOSYS);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn bpf_error_adapter_rules_hold() {
        assert!(super::bpf_error_adapter_rules_hold_for_test());
    }

    #[test]
    fn bpf_error_more_variants_and_edge_cases_hold() {
        assert!(super::bpf_error_more_variants_and_edge_cases_hold_for_test());
    }
}
