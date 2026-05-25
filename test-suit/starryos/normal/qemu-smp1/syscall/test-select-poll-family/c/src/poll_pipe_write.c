#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_pipe_write(void) {
    MODULE_START("poll_pipe_write");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[1], .events = POLLOUT, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "empty pipe POLLOUT returns 1");
    CHECK(pfd.revents & POLLOUT, "revents has POLLOUT");

    close(fds[0]);
    close(fds[1]);

    int fds2[2];
    CHECK_RET(create_nonblocking_pipe(fds2), 0, "nonblocking pipe created");
    fill_pipe(fds2[1]);
    pfd.fd = fds2[1];
    pfd.events = POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "full pipe POLLOUT returns 0 (timeout)");

    close(fds2[0]);
    close(fds2[1]);

    MODULE_SUMMARY("poll_pipe_write");
    MODULE_RETURN();
}
