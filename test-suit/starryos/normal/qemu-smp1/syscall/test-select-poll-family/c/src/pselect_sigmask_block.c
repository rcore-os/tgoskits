#include "test_framework.h"
#include "helpers.h"
#include <sys/select.h>
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <errno.h>

#define KERNEL_SIGSET_SIZE (sizeof(unsigned long))

static volatile sig_atomic_t usr1_received = 0;

static void sigusr1_handler(int sig) {
    (void)sig;
    usr1_received = 1;
}

int run_pselect_sigmask_block(void) {
    MODULE_START("pselect_sigmask_block");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");

    signal(SIGUSR1, sigusr1_handler);

    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);

    pid_t pid = fork();
    if (pid == 0) {
        close(fds[0]);
        usleep(50000);
        kill(getppid(), SIGUSR1);
        usleep(50000);
        char c = 'X';
        write(fds[1], &c, 1);
        close(fds[1]);
        _exit(0);
    }

    close(fds[1]);

    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fds[0], &rfds);

    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 500000000;

    struct {
        const sigset_t *ss;
        size_t ss_len;
    } sigmask_data = { &mask, KERNEL_SIGSET_SIZE };

    usr1_received = 0;

    int ret = (int)syscall(SYS_pselect6, fds[0] + 1, &rfds, NULL, NULL, &ts, &sigmask_data);

    CHECK(ret == 1, "pselect returns 1 (not interrupted by SIGUSR1)");
    CHECK(FD_ISSET(fds[0], &rfds), "FD_ISSET true for pipe read end");
    CHECK(usr1_received == 1, "SIGUSR1 handler was called");

    close(fds[0]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("pselect_sigmask_block");
    MODULE_RETURN();
}
