#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_write_pipe(void) {
    MODULE_START("select_write_pipe");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(fds[1], &wfds);
    struct timeval tv = {1, 0};
    int ret = raw_select(fds[1] + 1, NULL, &wfds, NULL, &tv);
    CHECK(ret == 1, "select write on empty pipe returns 1");
    CHECK(FD_ISSET(fds[1], &wfds), "FD_ISSET true for write fd");

    close(fds[0]);
    close(fds[1]);

    CHECK_RET(create_nonblocking_pipe(fds), 0, "nonblocking pipe created");
    fill_pipe(fds[1]);

    FD_ZERO(&wfds);
    FD_SET(fds[1], &wfds);
    tv.tv_sec = 0;
    tv.tv_usec = 50000;
    ret = raw_select(fds[1] + 1, NULL, &wfds, NULL, &tv);
    CHECK(ret == 0 || ret == 1, "select on full pipe returns 0 or 1");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("select_write_pipe");
    MODULE_RETURN();
}
