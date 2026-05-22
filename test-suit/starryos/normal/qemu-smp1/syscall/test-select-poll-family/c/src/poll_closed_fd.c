#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_closed_fd(void) {
    MODULE_START("poll_closed_fd");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    close(fds[0]);
    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    int ret = (int)raw_poll(&pfd, 1, 10);
    CHECK(ret == 1, "closed read fd returns 1");
    CHECK(pfd.revents & POLLNVAL, "revents has POLLNVAL for closed read fd");

    close(fds[1]);
    pfd.fd = fds[1];
    pfd.events = POLLOUT;
    pfd.revents = 0;
    ret = (int)raw_poll(&pfd, 1, 10);
    CHECK(ret == 1, "closed write fd returns 1");
    CHECK(pfd.revents & POLLNVAL, "revents has POLLNVAL for closed write fd");

    MODULE_SUMMARY("poll_closed_fd");
    MODULE_RETURN();
}
