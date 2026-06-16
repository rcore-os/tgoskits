#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <time.h>
#include <signal.h>
#include <sys/syscall.h>

int run_ppoll_err_einval(void) {
    MODULE_START("ppoll_err_einval");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };

    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = -1;
    CHECK_ERRNO(syscall(SYS_ppoll, &pfd, 1, &ts, NULL, 0), EINVAL, "ppoll tv_nsec=-1 returns EINVAL");

    ts.tv_sec = 0;
    ts.tv_nsec = 1000000000;
    pfd.revents = 0;
    CHECK_ERRNO(syscall(SYS_ppoll, &pfd, 1, &ts, NULL, 0), EINVAL, "ppoll tv_nsec=1000000000 returns EINVAL");

    ts.tv_sec = -1;
    ts.tv_nsec = 0;
    pfd.revents = 0;
    CHECK_ERRNO(syscall(SYS_ppoll, &pfd, 1, &ts, NULL, 0), EINVAL, "ppoll tv_sec=-1 returns EINVAL");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("ppoll_err_einval");
    MODULE_RETURN();
}
