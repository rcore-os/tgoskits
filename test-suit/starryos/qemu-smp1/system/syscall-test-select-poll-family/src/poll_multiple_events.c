#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_multiple_events(void) {
    MODULE_START("poll_multiple_events");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[1], .events = POLLIN | POLLOUT, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "write end POLLIN|POLLOUT returns 1");
    CHECK(pfd.revents & POLLOUT, "revents has POLLOUT");
    CHECK(!(pfd.revents & POLLIN), "revents does not have POLLIN");

    struct pollfd pfds[2];
    pfds[0].fd = fds[0];
    pfds[0].events = POLLIN;
    pfds[0].revents = 0;
    pfds[1].fd = fds[1];
    pfds[1].events = POLLOUT;
    pfds[1].revents = 0;
    CHECK_RET(raw_poll(pfds, 2, 10), 1, "read POLLIN + write POLLOUT returns 1");
    CHECK(pfds[1].revents & POLLOUT, "write end revents has POLLOUT");
    CHECK(pfds[0].revents == 0, "read end revents is 0");

    write_exact(fds[1], "A", 1);
    pfd.fd = fds[0];
    pfd.events = POLLIN | POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "read end with data POLLIN|POLLOUT returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("poll_multiple_events");
    MODULE_RETURN();
}
