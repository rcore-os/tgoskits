#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/syscall.h>
#include <sys/wait.h>

int run_poll_timeout_negative(void) {
    MODULE_START("poll_timeout_negative");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    pid_t pid = fork();
    if (pid == 0) {
        usleep(50000);
        write_exact(fds[1], "X", 1);
        close(fds[0]);
        close(fds[1]);
        _exit(0);
    }

    close(fds[1]);
    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    int ret = (int)raw_poll(&pfd, 1, -1);
    CHECK(ret == 1, "poll(timeout=-1) returns 1 after child writes");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");

    close(fds[0]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("poll_timeout_negative");
    MODULE_RETURN();
}
