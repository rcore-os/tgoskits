#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <signal.h>
#include <sys/wait.h>
#include <sys/syscall.h>

static volatile int sigusr1_received = 0;

static void sigusr1_handler(int sig) {
    (void)sig;
    sigusr1_received = 1;
}

int run_poll_err_eintr(void) {
    MODULE_START("poll_err_eintr");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    struct sigaction sa;
    sa.sa_handler = sigusr1_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGUSR1, &sa, NULL);
    sigusr1_received = 0;

    pid_t pid = fork();
    if (pid == 0) {
        usleep(30000);
        kill(getppid(), SIGUSR1);
        close(fds[0]);
        close(fds[1]);
        _exit(0);
    }

    struct pollfd pfd = { .fd = fds[0], .events = POLLIN, .revents = 0 };
    errno = 0;
    long ret = raw_poll(&pfd, 1, 200);
    CHECK(ret == -1 && errno == EINTR, "poll interrupted by SIGUSR1 returns EINTR");
    CHECK(sigusr1_received == 1, "SIGUSR1 handler was called");

    int status;
    waitpid(pid, &status, 0);

    close(fds[0]);
    close(fds[1]);

    int fds2[2];
    CHECK_RET(create_pipe(fds2), 0, "pipe2 created");

    sigusr1_received = 0;
    pid = fork();
    if (pid == 0) {
        usleep(30000);
        kill(getppid(), SIGUSR1);
        close(fds2[0]);
        close(fds2[1]);
        _exit(0);
    }

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds2[0], &rfds);
    struct timeval tv = {1, 0};
    errno = 0;
    ret = raw_select(fds2[0] + 1, &rfds, NULL, NULL, &tv);
    CHECK(ret == -1 && errno == EINTR, "select interrupted by SIGUSR1 returns EINTR");
    CHECK(sigusr1_received == 1, "SIGUSR1 handler was called for select");

    waitpid(pid, &status, 0);

    close(fds2[0]);
    close(fds2[1]);

    MODULE_SUMMARY("poll_err_eintr");
    MODULE_RETURN();
}
