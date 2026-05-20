#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_pipe_hup(void) {
    MODULE_START("poll_pipe_hup");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created for test 1");
    close(fds[1]);
    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 100), 1, "read end POLLIN after close write returns 1");
    CHECK(pfd.revents & POLLHUP, "revents has POLLHUP");
    close(fds[0]);

    CHECK_RET(create_pipe(fds), 0, "pipe created for test 2");
    write_exact(fds[1], "HELLO", 5);
    close(fds[1]);
    pfd.fd = fds[0];
    pfd.events = POLLIN;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 100), 1, "read end with data + closed write returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");
    CHECK(pfd.revents & POLLHUP, "revents has POLLHUP");

    char buf[16];
    read_exact(fds[0], buf, 5);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 100), 1, "read end after drain + closed write returns 1");
    CHECK(pfd.revents & POLLHUP, "revents has POLLHUP after drain");
    CHECK(!(pfd.revents & POLLIN), "revents does not have POLLIN after drain");
    close(fds[0]);

    CHECK_RET(create_pipe(fds), 0, "pipe created for test 4");
    close(fds[0]);
    pfd.fd = fds[1];
    pfd.events = POLLOUT;
    pfd.revents = 0;
    int ret = (int)raw_poll(&pfd, 1, 100);
    CHECK(ret >= 1, "write end after close read returns >=1");
    CHECK(pfd.revents & POLLERR, "revents has POLLERR");
    close(fds[1]);

    MODULE_SUMMARY("poll_pipe_hup");
    MODULE_RETURN();
}
