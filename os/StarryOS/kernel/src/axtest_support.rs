//! Isolated axtest coverage for StarryOS kernel internals.
//!
//! This module is compiled only for the dedicated `starryos-axtest-kernel`
//! test binary. It is intentionally not part of the normal StarryOS boot path.

use axtest::prelude::*;

/// Keep this module reachable from the dedicated axtest binary so its linker
/// section descriptors are retained.
pub fn link() {}

#[axtest]
fn user_stack_layout_is_inside_user_space() {
    ax_assert!(super::config::USER_SPACE_BASE < super::config::USER_STACK_TOP);
    ax_assert!(super::config::USER_STACK_SIZE > 0);
    ax_assert!(
        super::config::USER_STACK_TOP
            <= super::config::USER_SPACE_BASE + super::config::USER_SPACE_SIZE
    );
}

#[axtest]
fn signal_trampoline_is_page_aligned() {
    ax_assert_eq!(super::config::SIGNAL_TRAMPOLINE & 0xfff, 0);
}

#[axtest]
fn timespec_rejects_invalid_nsec() {
    use super::time::TimeValueLike;

    let invalid = linux_raw_sys::general::__kernel_timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    ax_assert!(invalid.try_into_time_value().is_err());
}
