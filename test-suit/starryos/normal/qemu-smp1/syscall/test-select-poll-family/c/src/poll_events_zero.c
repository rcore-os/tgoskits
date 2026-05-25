#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_events_zero(void) {
    MODULE_START("poll_events_zero");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[0], .events = 0, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "read end events=0 returns 0");

    pfd.fd = fds[1];
    pfd.events = 0;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "write end events=0 returns 0");

    close(fds[0]);
    close(fds[1]);

    CHECK_RET(create_pipe(fds), 0, "pipe created for POLLHUP test");
    close(fds[1]);

    pfd.fd = fds[0];
    pfd.events = 0;
    pfd.revents = 0;
    int ret = (int)raw_poll(&pfd, 1, 100);
    CHECK(ret >= 1, "events=0 with closed write end returns >=1");
    CHECK(pfd.revents & POLLHUP, "revents has POLLHUP even when events=0");

    close(fds[0]);

    MODULE_SUMMARY("poll_events_zero");
    MODULE_RETURN();
}
