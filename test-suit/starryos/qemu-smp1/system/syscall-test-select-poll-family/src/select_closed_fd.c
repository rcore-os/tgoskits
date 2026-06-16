#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_select_closed_fd(void) {
    MODULE_START("select_closed_fd");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    int read_fd = fds[0];
    close(read_fd);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(read_fd, &rfds);
    struct timeval tv = {1, 0};
    CHECK_ERRNO(raw_select(read_fd + 1, &rfds, NULL, NULL, &tv), EBADF, "select closed read fd returns EBADF");

    close(fds[1]);

    int fds2[2];
    CHECK_RET(create_pipe(fds2), 0, "pipe2 created");

    close(fds2[1]);

    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(fds2[1], &wfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    CHECK_ERRNO(raw_select(fds2[1] + 1, NULL, &wfds, NULL, &tv), EBADF, "select closed write fd returns EBADF");

    close(fds2[0]);

    MODULE_SUMMARY("select_closed_fd");
    MODULE_RETURN();
}
