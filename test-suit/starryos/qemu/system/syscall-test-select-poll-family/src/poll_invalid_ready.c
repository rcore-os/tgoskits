#include "test_framework.h"
#include "helpers.h"
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

int run_poll_invalid_ready(void) {
    MODULE_START("poll_invalid_ready");

    int pipe_fds[2];
    CHECK_RET(create_pipe(pipe_fds), 0, "pipe created");
    CHECK_RET(write_exact(pipe_fds[1], "A", 1), 0, "pipe made readable");

    struct pollfd poll_fds[2] = {
        { .fd = INT_MAX, .events = POLLOUT, .revents = 0 },
        { .fd = pipe_fds[0], .events = POLLIN, .revents = 0 },
    };
    CHECK_RET(raw_poll(poll_fds, 2, 0), 2,
              "poll counts invalid and ready descriptors");
    CHECK(poll_fds[0].revents == POLLNVAL,
          "poll reports POLLNVAL for invalid descriptor");
    CHECK(poll_fds[1].revents == POLLIN,
          "poll reports POLLIN for ready descriptor");

    poll_fds[0].revents = 0;
    poll_fds[1].revents = 0;
    struct timespec timeout = { .tv_sec = 0, .tv_nsec = 0 };
    CHECK_RET(syscall(SYS_ppoll, poll_fds, 2, &timeout, NULL, 8), 2,
              "ppoll counts invalid and ready descriptors");
    CHECK(poll_fds[0].revents == POLLNVAL,
          "ppoll reports POLLNVAL for invalid descriptor");
    CHECK(poll_fds[1].revents == POLLIN,
          "ppoll reports POLLIN for ready descriptor");

    close(pipe_fds[0]);
    close(pipe_fds[1]);

    MODULE_SUMMARY("poll_invalid_ready");
    MODULE_RETURN();
}
