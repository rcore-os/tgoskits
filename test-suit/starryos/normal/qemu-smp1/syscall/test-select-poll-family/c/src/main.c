#include "test_framework.h"
#include <stdio.h>
#include <stdlib.h>

extern int run_select_zero_timeout(void);
extern int run_select_null_timeout(void);
extern int run_select_empty_all(void);
extern int run_select_read_pipe(void);
extern int run_select_write_pipe(void);
extern int run_select_rw_simultaneous(void);
extern int run_select_exceptfds(void);
extern int run_select_multiple_fds(void);
extern int run_select_fd_set_cleared(void);
extern int run_select_must_reinit(void);
extern int run_select_nfds_semantics(void);
extern int run_select_regular_file(void);
extern int run_select_devnull(void);
extern int run_select_closed_fd(void);
extern int run_select_bad_nfds(void);

extern int run_poll_timeout_zero(void);
extern int run_poll_timeout_negative(void);
extern int run_poll_null_fds(void);
extern int run_poll_pipe_read(void);
extern int run_poll_pipe_write(void);
extern int run_poll_multiple_events(void);
extern int run_poll_negative_fd(void);
extern int run_poll_events_zero(void);
extern int run_poll_multiple_fds(void);
extern int run_poll_regular_file(void);
extern int run_poll_closed_fd(void);
extern int run_poll_pipe_hup(void);

extern int run_pselect_sigmask_block(void);
extern int run_pselect_sigmask_restore(void);
extern int run_pselect_no_sigmask(void);
extern int run_ppoll_sigmask_block(void);
extern int run_ppoll_sigmask_restore(void);
extern int run_ppoll_no_sigmask(void);

extern int run_select_err_ebadf(void);
extern int run_select_err_einval(void);
extern int run_poll_err_einval(void);
extern int run_ppoll_err_einval(void);
extern int run_poll_err_eintr(void);

extern int run_poll_events_matrix(void);
extern int run_select_fds_bits_matrix(void);
extern int run_stress_many_fds(void);
extern int run_stress_loop(void);
extern int run_timeout_precision(void);

static int total_pass = 0;
static int total_fail = 0;

static void run_module(const char *name, int (*fn)(void)) {
    int fails = fn();
    if (fails == 0) {
        total_pass++;
        printf("  [PASS] module: %s\n", name);
    } else {
        total_fail++;
        printf("  [FAIL] module: %s (%d failures)\n", name, fails);
    }
}

int main(void) {
    printf("================================================\n");
    printf("  TEST: select/poll/pselect6/ppoll deep suite\n");
    printf("  43 modules, ~300+ checkpoints\n");
    printf("================================================\n");

    printf("\n=== Phase 1: select basics ===\n");
    run_module("select_zero_timeout",      run_select_zero_timeout);
    run_module("select_null_timeout",      run_select_null_timeout);
    run_module("select_empty_all",         run_select_empty_all);
    run_module("select_read_pipe",         run_select_read_pipe);
    run_module("select_write_pipe",        run_select_write_pipe);
    run_module("select_rw_simultaneous",   run_select_rw_simultaneous);
    run_module("select_exceptfds",         run_select_exceptfds);
    run_module("select_multiple_fds",      run_select_multiple_fds);
    run_module("select_fd_set_cleared",    run_select_fd_set_cleared);
    run_module("select_must_reinit",       run_select_must_reinit);
    run_module("select_nfds_semantics",    run_select_nfds_semantics);
    run_module("select_regular_file",      run_select_regular_file);
    run_module("select_devnull",           run_select_devnull);
    run_module("select_closed_fd",         run_select_closed_fd);
    run_module("select_bad_nfds",          run_select_bad_nfds);

    printf("\n=== Phase 2: poll basics ===\n");
    run_module("poll_timeout_zero",        run_poll_timeout_zero);
    run_module("poll_timeout_negative",    run_poll_timeout_negative);
    run_module("poll_null_fds",            run_poll_null_fds);
    run_module("poll_pipe_read",           run_poll_pipe_read);
    run_module("poll_pipe_write",          run_poll_pipe_write);
    run_module("poll_multiple_events",     run_poll_multiple_events);
    run_module("poll_negative_fd",         run_poll_negative_fd);
    run_module("poll_events_zero",         run_poll_events_zero);
    run_module("poll_multiple_fds",        run_poll_multiple_fds);
    run_module("poll_regular_file",        run_poll_regular_file);
    run_module("poll_closed_fd",           run_poll_closed_fd);
    run_module("poll_pipe_hup",            run_poll_pipe_hup);

    printf("\n=== Phase 3: signal interaction ===\n");
    run_module("pselect_sigmask_block",    run_pselect_sigmask_block);
    run_module("pselect_sigmask_restore",  run_pselect_sigmask_restore);
    run_module("pselect_no_sigmask",       run_pselect_no_sigmask);
    run_module("ppoll_sigmask_block",      run_ppoll_sigmask_block);
    run_module("ppoll_sigmask_restore",    run_ppoll_sigmask_restore);
    run_module("ppoll_no_sigmask",         run_ppoll_no_sigmask);

    printf("\n=== Phase 4: error paths ===\n");
    run_module("select_err_ebadf",         run_select_err_ebadf);
    run_module("select_err_einval",        run_select_err_einval);
    run_module("poll_err_einval",          run_poll_err_einval);
    run_module("ppoll_err_einval",         run_ppoll_err_einval);
    run_module("poll_err_eintr",           run_poll_err_eintr);

    printf("\n=== Phase 5: matrix & stress ===\n");
    run_module("poll_events_matrix",       run_poll_events_matrix);
    run_module("select_fds_bits_matrix",   run_select_fds_bits_matrix);
    run_module("stress_many_fds",          run_stress_many_fds);
    run_module("stress_loop",              run_stress_loop);
    run_module("timeout_precision",        run_timeout_precision);

    printf("\n================================================\n");
    printf("  SUMMARY: %d modules passed, %d modules failed\n",
           total_pass, total_fail);
    printf("================================================\n");

    return total_fail > 0 ? 1 : 0;
}
