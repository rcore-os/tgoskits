#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <errno.h>

#define KERNEL_SIGSET_SIZE (sizeof(unsigned long))

int run_ppoll_sigmask_restore(void) {
    MODULE_START("ppoll_sigmask_restore");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    sigset_t before;
    sigemptyset(&before);
    sigprocmask(SIG_SETMASK, NULL, &before);

    sigset_t ppoll_mask;
    sigemptyset(&ppoll_mask);
    sigaddset(&ppoll_mask, SIGUSR1);

    pid_t pid = fork();
    if (pid == 0) {
        close(fds[0]);
        usleep(20000);
        char c = 'R';
        write(fds[1], &c, 1);
        close(fds[1]);
        _exit(0);
    }

    close(fds[1]);

    struct pollfd pfd;
    pfd.fd = fds[0];
    pfd.events = POLLIN;
    pfd.revents = 0;

    struct timespec ts;
    ts.tv_sec = 1;
    ts.tv_nsec = 0;

    int ret = (int)syscall(SYS_ppoll, &pfd, 1, &ts, &ppoll_mask, KERNEL_SIGSET_SIZE);
    CHECK(ret == 1, "ppoll returns 1 (pipe readable)");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");

    sigset_t after;
    sigemptyset(&after);
    sigprocmask(SIG_SETMASK, NULL, &after);

    CHECK(sigismember(&after, SIGUSR1) == 0, "SIGUSR1 not in signal mask after ppoll");
    CHECK(sigismember(&before, SIGUSR1) == sigismember(&after, SIGUSR1),
          "signal mask restored to original state");

    close(fds[0]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("ppoll_sigmask_restore");
    MODULE_RETURN();
}
