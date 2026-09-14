#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/signalfd.h>
#include <sys/syscall.h>
#include <unistd.h>

static int fail(const char *operation)
{
    fprintf(stderr, "FAIL: %s: errno=%d (%s)\n", operation, errno,
            strerror(errno));
    return EXIT_FAILURE;
}

int main(void)
{
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    sigaddset(&mask, SIGUSR2);

    if (sigprocmask(SIG_BLOCK, &mask, NULL) != 0) {
        return fail("sigprocmask");
    }

    uint64_t kernel_mask = (UINT64_C(1) << (SIGUSR1 - 1)) |
                           (UINT64_C(1) << (SIGUSR2 - 1));
    int signal_fd = (int)syscall(SYS_signalfd4, -1, &kernel_mask,
                                 sizeof(kernel_mask), SFD_NONBLOCK);
    if (signal_fd < 0) {
        return fail("signalfd4");
    }

    if (kill(getpid(), SIGUSR1) != 0 || kill(getpid(), SIGUSR2) != 0) {
        close(signal_fd);
        return fail("kill");
    }

    struct signalfd_siginfo infos[2];
    memset(infos, 0, sizeof(infos));
    errno = 0;
    ssize_t result = read(signal_fd, infos, sizeof(infos));
    int read_errno = errno;
    close(signal_fd);

    if (result != (ssize_t)sizeof(infos) || read_errno != 0 ||
        infos[0].ssi_signo != SIGUSR1 || infos[1].ssi_signo != SIGUSR2) {
        fprintf(stderr,
                "FAIL: batch read: result=%zd errno=%d signs=%u,%u, expected 256/0/%d/%d\n",
                result, read_errno, infos[0].ssi_signo, infos[1].ssi_signo,
                SIGUSR1, SIGUSR2);
        return EXIT_FAILURE;
    }

    puts("PASS: signalfd read returns all fitting pending records");
    puts("STARRY_SIGNALFD_BATCH_READ_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-signalfd-batch-read");
    return EXIT_SUCCESS;
}
