#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required"
#endif
#ifndef __NR_pidfd_send_signal
#error "__NR_pidfd_send_signal required"
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static int x_pidfd_send_signal(int pidfd, int sig, siginfo_t *info, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_send_signal, pidfd, sig, info, flags);
}

static void test_pidfd_send_signal_paths(void)
{
    printf("--- pidfd_send_signal ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid()) 成功");
    if (pfd < 0) {
        return;
    }

    CHECK_ERR(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 1u), EINVAL,
              "flags 非零 -> EINVAL");

    CHECK_RET(x_pidfd_send_signal(pfd, 0, NULL, 0), 0, "signo==0 空 info 成功 (no-op)");

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = SIG_IGN;
    CHECK_RET(sigaction(SIGUSR1, &sa, NULL), 0, "忽略 SIGUSR1");

    CHECK_RET(x_pidfd_send_signal(pfd, SIGUSR1, NULL, 0), 0,
              "SIGUSR1 + NULL info 成功 (已忽略)");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

int main(void)
{
    TEST_START("pidfd_send_signal");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_send_signal_paths();

    TEST_DONE();
}
