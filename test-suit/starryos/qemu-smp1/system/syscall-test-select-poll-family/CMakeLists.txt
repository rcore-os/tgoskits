cmake_minimum_required(VERSION 3.20)
project(test-select-poll-family C)
set(APP_NAME test-select-poll-family)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(SOURCES
    src/main.c
    src/helpers.c
    src/select_zero_timeout.c
    src/select_null_timeout.c
    src/select_empty_all.c
    src/select_read_pipe.c
    src/select_write_pipe.c
    src/select_rw_simultaneous.c
    src/select_exceptfds.c
    src/select_multiple_fds.c
    src/select_fd_set_cleared.c
    src/select_must_reinit.c
    src/select_nfds_semantics.c
    src/select_regular_file.c
    src/select_devnull.c
    src/select_closed_fd.c
    src/select_bad_nfds.c
    src/poll_timeout_zero.c
    src/poll_timeout_negative.c
    src/poll_null_fds.c
    src/poll_pipe_read.c
    src/poll_pipe_write.c
    src/poll_multiple_events.c
    src/poll_negative_fd.c
    src/poll_events_zero.c
    src/poll_multiple_fds.c
    src/poll_regular_file.c
    src/poll_closed_fd.c
    src/poll_pipe_hup.c
    src/pselect_sigmask_block.c
    src/pselect_sigmask_restore.c
    src/pselect_no_sigmask.c
    src/ppoll_sigmask_block.c
    src/ppoll_sigmask_restore.c
    src/ppoll_no_sigmask.c
    src/select_err_ebadf.c
    src/select_err_einval.c
    src/poll_err_einval.c
    src/ppoll_err_einval.c
    src/poll_err_eintr.c
    src/poll_events_matrix.c
    src/select_fds_bits_matrix.c
    src/stress_many_fds.c
    src/stress_loop.c
    src/timeout_precision.c
)

add_executable(${APP_NAME} ${SOURCES})
target_include_directories(${APP_NAME} PRIVATE src)
target_compile_options(${APP_NAME} PRIVATE -Wall -Wextra -Werror)
target_compile_definitions(${APP_NAME} PRIVATE _GNU_SOURCE)
install(TARGETS ${APP_NAME} RUNTIME DESTINATION usr/bin/starry-test-suit)
