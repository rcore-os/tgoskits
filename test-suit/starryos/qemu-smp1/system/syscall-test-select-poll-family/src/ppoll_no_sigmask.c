#include "test_framework.h"
#include "helpers.h"
#include <poll.h>
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <errno.h>
#include <string.h>

static volatile sig_atomic_t usr1_received = 0;
static int sig_pipe[2] = {-1, -1};

static void sigusr1_handler(int sig) {
    (void)sig;
    usr1_received = 1;
    if (sig_pipe[1] >= 0) {
        char c = 'S';
        write(sig_pipe[1], &c, 1);
    }
}

int run_ppoll_no_sigmask(void) {
    MODULE_START("ppoll_no_sigmask");

    int fds[2];
    CHECK_RET(create_pipe(fds), 0, "pipe created");
    CHECK_RET(pipe(sig_pipe), 0, "signal pipe created");

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigusr1_handler;
    sa.sa_flags = 0;
    sigaction(SIGUSR1, &sa, NULL);

    pid_t pid = fork();
    if (pid == 0) {
        close(fds[0]);
        close(sig_pipe[0]);
        close(sig_pipe[1]);
        usleep(50000);
        kill(getppid(), SIGUSR1);
        close(fds[1]);
        _exit(0);
    }

    close(fds[1]);

    struct pollfd pfds[2];
    pfds[0].fd = fds[0];
    pfds[0].events = POLLIN;
    pfds[0].revents = 0;
    pfds[1].fd = sig_pipe[0];
    pfds[1].events = POLLIN;
    pfds[1].revents = 0;

    struct timespec ts;
    ts.tv_sec = 3;
    ts.tv_nsec = 0;

    usr1_received = 0;

    int ret = (int)syscall(SYS_ppoll, pfds, 2, &ts, NULL, 8);

    CHECK(ret == -1 || ret >= 1, "ppoll returns -1 (EINTR) or >=1 (sig_pipe readable)");
    if (ret == -1) {
        CHECK(errno == EINTR, "errno is EINTR on signal interrupt");
    }
    CHECK(usr1_received == 1, "SIGUSR1 handler was invoked");

    close(fds[0]);
    close(sig_pipe[0]);
    close(sig_pipe[1]);
    int status;
    waitpid(pid, &status, 0);

    MODULE_SUMMARY("ppoll_no_sigmask");
    MODULE_RETURN();
}
