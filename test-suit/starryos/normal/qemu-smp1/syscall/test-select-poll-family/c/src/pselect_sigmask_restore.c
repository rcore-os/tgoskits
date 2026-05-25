#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <errno.h>

#define KERNEL_SIGSET_SIZE (sizeof(unsigned long))

int run_pselect_sigmask_restore(void) {
    MODULE_START("pselect_sigmask_restore");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    sigset_t before;
    sigemptyset(&before);
    sigprocmask(SIG_SETMASK, NULL, &before);

    sigset_t pselect_mask;
    sigemptyset(&pselect_mask);
    sigaddset(&pselect_mask, SIGUSR1);

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

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);

    struct timespec ts;
    ts.tv_sec = 1;
    ts.tv_nsec = 0;

    struct {
        const sigset_t *ss;
        size_t ss_len;
    } sigmask_data = { &pselect_mask, KERNEL_SIGSET_SIZE };

    int ret = (int)syscall(SYS_pselect6, fds[0] + 1, &rfds, NULL, NULL, &ts, &sigmask_data);
    CHECK(ret == 1, "pselect6 returns 1 (pipe readable)");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true for pipe read end");

    sigset_t after;
    sigemptyset(&after);
    sigprocmask(SIG_SETMASK, NULL, &after);

    CHECK(sigismember(&after, SIGUSR1) == 0, "SIGUSR1 not in signal mask after pselect");
    CHECK(sigismember(&before, SIGUSR1) == sigismember(&after, SIGUSR1),
          "signal mask restored to original state");

    close(fds[0]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("pselect_sigmask_restore");
    MODULE_RETURN();
}
