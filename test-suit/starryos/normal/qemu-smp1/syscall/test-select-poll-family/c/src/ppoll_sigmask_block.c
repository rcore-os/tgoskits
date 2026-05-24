#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
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

int run_ppoll_sigmask_block(void) {
    MODULE_START("ppoll_sigmask_block");

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

    struct pollfd pfd;
    pfd.fd = fds[0];
    pfd.events = POLLIN;
    pfd.revents = 0;

    struct timespec ts;
    ts.tv_sec = 0;
    ts.tv_nsec = 500000000;

    usr1_received = 0;

    int ret = (int)syscall(SYS_ppoll, &pfd, 1, &ts, &mask, KERNEL_SIGSET_SIZE);

    CHECK(ret == 1, "ppoll returns 1 (not interrupted by SIGUSR1)");
    CHECK(pfd.revents & POLLIN, "revents has POLLIN");
    CHECK(usr1_received == 1, "SIGUSR1 handler was called");

    close(fds[0]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("ppoll_sigmask_block");
    MODULE_RETURN();
}
