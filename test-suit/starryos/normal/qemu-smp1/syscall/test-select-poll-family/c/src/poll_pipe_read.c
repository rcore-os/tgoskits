#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_pipe_read(void) {
    MODULE_START("poll_pipe_read");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "empty pipe POLLIN timeout returns 0");

    write_exact(fds[1], "A", 1);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "pipe with 1 byte returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");

    char buf[16];
    read_exact(fds[0], buf, 1);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 0, "pipe after read returns 0 (timeout)");

    char wbuf[256];
    for (int i = 0; i < 256; i++) wbuf[i] = (char)i;
    write_exact(fds[1], wbuf, 256);
    read_exact(fds[0], buf, 1);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 10), 1, "pipe with 255 remaining bytes returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN after partial read");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("poll_pipe_read");
    MODULE_RETURN();
}
