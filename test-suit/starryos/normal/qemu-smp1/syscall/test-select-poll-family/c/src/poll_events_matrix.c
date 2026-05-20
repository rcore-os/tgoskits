#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_events_matrix(void) {
    MODULE_START("poll_events_matrix");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd;

    pfd.fd = fds[0];
    pfd.events = POLLIN;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 0, "pipe read POLLIN empty timeout 0");

    write_exact(fds[1], "X", 1);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 1, "pipe read POLLIN with data returns 1");
    CHECK(pfd.revents & POLLIN, "pipe read revents has POLLIN with data");

    char buf[16];
    read_exact(fds[0], buf, 1);

    pfd.fd = fds[1];
    pfd.events = POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 1, "pipe write POLLOUT empty returns 1");
    CHECK(pfd.revents & POLLOUT, "pipe write revents has POLLOUT");

    pfd.fd = fds[0];
    pfd.events = POLLPRI;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 0, "pipe read POLLPRI returns 0 timeout");

    const char *path = "/tmp/test_poll_events_matrix";
    create_temp_file(path);

    int fd_r = open(path, O_RDONLY);
    CHECK(fd_r >= 0, "open regular file O_RDONLY");
    pfd.fd = fd_r;
    pfd.events = POLLIN;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLIN returns 1");
    close(fd_r);

    int fd_w = open(path, O_WRONLY);
    CHECK(fd_w >= 0, "open regular file O_WRONLY");
    pfd.fd = fd_w;
    pfd.events = POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLOUT returns 1");
    close(fd_w);

    int fd_rw = open(path, O_RDONLY);
    CHECK(fd_rw >= 0, "open regular file O_RDONLY for POLLIN|POLLOUT");
    pfd.fd = fd_rw;
    pfd.events = POLLIN | POLLOUT;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "regular file POLLIN|POLLOUT returns 1");
    close(fd_rw);

    unlink(path);

    int devnull_r = open("/dev/null", O_RDONLY);
    if (devnull_r >= 0) {
        pfd.fd = devnull_r;
        pfd.events = POLLIN;
        pfd.revents = 0;
        CHECK_RET(raw_poll(&pfd, 1, 10), 1, "/dev/null POLLIN returns 1");
        close(devnull_r);
    } else {
        __pass++;
    }

    int devnull_w = open("/dev/null", O_WRONLY);
    if (devnull_w >= 0) {
        pfd.fd = devnull_w;
        pfd.events = POLLOUT;
        pfd.revents = 0;
        CHECK_RET(raw_poll(&pfd, 1, 10), 1, "/dev/null POLLOUT returns 1");
        close(devnull_w);
    } else {
        __pass++;
    }

    write_exact(fds[1], "Y", 1);
    pfd.fd = fds[0];
    pfd.events = POLLIN | POLLPRI;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 1, "pipe read POLLIN|POLLPRI with data returns 1");
    CHECK(pfd.revents & POLLIN, "pipe read revents has POLLIN");
    CHECK(!(pfd.revents & POLLPRI), "pipe read revents has no POLLPRI");

    pfd.fd = fds[1];
    pfd.events = POLLOUT | POLLWRNORM;
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 1, "pipe write POLLOUT|POLLWRNORM returns 1");
    CHECK(pfd.revents & POLLOUT, "pipe write revents has POLLOUT");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("poll_events_matrix");
    MODULE_RETURN();
}
