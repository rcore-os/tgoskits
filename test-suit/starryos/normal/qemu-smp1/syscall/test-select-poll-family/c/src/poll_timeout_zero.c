#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>

int run_poll_timeout_zero(void) {
    MODULE_START("poll_timeout_zero");

    CHECK_RET(raw_poll(NULL, 0, 0), 0, "poll(NULL,0,0) returns 0");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    CHECK_RET(raw_poll(&pfd, 1, 0), 0, "poll empty pipe timeout=0 returns 0");

    write_exact(fds[1], "A", 1);
    pfd.revents = 0;
    CHECK_RET(raw_poll(&pfd, 1, 0), 1, "poll pipe with data timeout=0 returns 1");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");

    close(fds[0]);
    close(fds[1]);

    MODULE_SUMMARY("poll_timeout_zero");
    MODULE_RETURN();
}
