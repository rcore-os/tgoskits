#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/resource.h>
#include <sys/syscall.h>

int run_select_err_ebadf(void) {
    MODULE_START("select_err_ebadf");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe1 created");

    close(fds[0]);
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);
    struct timeval tv = {1, 0};
    CHECK_ERRNO(raw_select(fds[0] + 1, &rfds, NULL, NULL, &tv), EBADF, "closed read fd in readfds returns EBADF");

    close(fds[1]);

    int fds2[2];
    CHECK_RET(create_pipe(fds2), 0, "pipe2 created");

    close(fds2[1]);
    fd_set wfds;
    FD_ZERO(&wfds);
    FD_SET(fds2[1], &wfds);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    CHECK_ERRNO(raw_select(fds2[1] + 1, NULL, &wfds, NULL, &tv), EBADF, "closed write fd in writefds returns EBADF");

    close(fds2[0]);

    int fds3[2];
    CHECK_RET(create_pipe(fds3), 0, "pipe3 created");

    close(fds3[0]);
    close(fds3[1]);
    fd_set rfds3;
    FD_ZERO(&rfds3);
    FD_SET(fds3[0], &rfds3);
    FD_SET(fds3[1], &rfds3);
    tv.tv_sec = 1;
    tv.tv_usec = 0;
    CHECK_ERRNO(raw_select(fds3[1] + 1, &rfds3, NULL, NULL, &tv), EBADF, "both closed fds in readfds returns EBADF");

    errno = 0;
    long ret2 = raw_select(FD_SETSIZE + 1, NULL, NULL, NULL, &tv);
    CHECK(ret2 == 0 || (ret2 == -1 && (errno == EINVAL || errno == ENOMEM)),
          "nfds > FD_SETSIZE with NULL fds: 0 (timeout) or error");

    MODULE_SUMMARY("select_err_ebadf");
    MODULE_RETURN();
}
