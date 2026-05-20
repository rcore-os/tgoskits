#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_regular_file(void) {
    MODULE_START("poll_regular_file");

    const char *path = "/tmp/test_poll_regular";
    create_temp_file(path);

    int fd_r = open(path, O_RDONLY);
    CHECK(fd_r >= 0, "open O_RDONLY");
    struct pollfd pfd = { .fd = fd_r, .events = POLLIN, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLIN returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");
    close(fd_r);

    int fd_w = open(path, O_WRONLY);
    CHECK(fd_w >= 0, "open O_WRONLY");
    pfd.fd = fd_w;
    pfd.events = POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLOUT returns 1");
    CHECK(pfd.revents & POLLOUT, "revents has POLLOUT");
    close(fd_w);

    fd_r = open(path, O_RDONLY);
    CHECK(fd_r >= 0, "open O_RDONLY for POLLIN|POLLOUT");
    pfd.fd = fd_r;
    pfd.events = POLLIN | POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLIN|POLLOUT returns 1");
    CHECK((pfd.revents & POLLIN) && (pfd.revents & POLLOUT), "revents has POLLIN|POLLOUT");
    close(fd_r);

    unlink(path);

    MODULE_SUMMARY("poll_regular_file");
    MODULE_RETURN();
}
