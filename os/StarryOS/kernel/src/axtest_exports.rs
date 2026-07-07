//! Narrow test-only exports for kernel axtest targets.

pub fn user_space_base() -> usize {
    super::config::USER_SPACE_BASE
}

pub fn user_space_size() -> usize {
    super::config::USER_SPACE_SIZE
}

pub fn user_stack_top() -> usize {
    super::config::USER_STACK_TOP
}

pub fn user_stack_size() -> usize {
    super::config::USER_STACK_SIZE
}

pub fn signal_trampoline() -> usize {
    super::config::SIGNAL_TRAMPOLINE
}

pub fn invalid_timespec_is_rejected() -> bool {
    use super::time::TimeValueLike;

    let invalid = linux_raw_sys::general::__kernel_timespec {
        tv_sec: 0,
        tv_nsec: 1_000_000_000,
    };
    invalid.try_into_time_value().is_err()
}

pub fn random_write_mixes_entropy() -> bool {
    super::pseudofs::dev::random_write_mixes_entropy_for_test()
}

pub fn pipe_peer_close_with_multiple_readers_is_visible() -> bool {
    super::file::peer_close_with_multiple_readers_is_visible_for_test()
}

pub fn pipe_resize_rejects_oversized_pipe() -> bool {
    super::file::resize_rejects_oversized_pipe_for_test()
}

pub fn fcntl_setpipe_size_returns_capacity() -> bool {
    super::syscall::fcntl_setpipe_size_returns_capacity_for_test()
}
