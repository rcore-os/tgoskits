#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use ax_std as _;
use starry_kernel::axtest_exports;

#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    use super::axtest_exports;

    #[test]
    fn user_stack_layout_is_inside_user_space() {
        ax_assert!(axtest_exports::user_space_base() < axtest_exports::user_stack_top());
        ax_assert!(axtest_exports::user_stack_size() > 0);
        ax_assert!(
            axtest_exports::user_stack_top()
                <= axtest_exports::user_space_base() + axtest_exports::user_space_size()
        );
    }

    #[test]
    fn signal_trampoline_is_page_aligned() {
        ax_assert_eq!(axtest_exports::signal_trampoline() & 0xfff, 0);
    }

    #[test]
    fn timespec_rejects_invalid_nsec() {
        ax_assert!(axtest_exports::invalid_timespec_is_rejected());
    }

    #[test]
    fn random_write_mixes_entropy() {
        ax_assert!(axtest_exports::random_write_mixes_entropy());
    }
}
