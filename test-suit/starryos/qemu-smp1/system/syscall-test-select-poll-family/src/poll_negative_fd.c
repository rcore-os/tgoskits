#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_negative_fd(void) {
    MODULE_START("poll_negative_fd");

    struct pollfd pfd = { .fd = -1, .events = POLLIN, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "poll fd=-1 returns 0");
    CHECK(pfd.revents == 0, "fd=-1 revents is 0");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");
    write_exact(fds[1], "A", 1);

    struct pollfd pfds[2];
    pfds[0].fd = fds[0];
    pfds[0].events = POLLIN;
    pfds[0].revents = 0;
    pfds[1].fd = -1;
    pfds[1].events = POLLIN;
    pfds[1].revents = 0;
    CHECK_RET(raw_poll(pfds, 2, 10), 1, "mixed valid+invalid returns 1");
    CHECK(pfds[0].revents & POLLIN, "valid fd revents has POLLIN");
    CHECK(pfds[1].revents == 0, "fd=-1 revents is 0");

    pfd.fd = -1;
    pfd.events = POLLIN | POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "fd=-1 POLLIN|POLLOUT returns 0");
    CHECK(pfd.revents == 0, "fd=-1 POLLIN|POLLOUT revents is 0");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("poll_negative_fd");
    MODULE_RETURN();
}
