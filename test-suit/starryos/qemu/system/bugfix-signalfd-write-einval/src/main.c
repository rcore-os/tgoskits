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

int main(void)
{
    uint64_t mask = UINT64_C(1) << (SIGUSR1 - 1);
    int signal_fd = (int)syscall(SYS_signalfd4, -1, &mask,
                                 sizeof(mask), SFD_NONBLOCK);
    if (signal_fd < 0) {
        fprintf(stderr, "FAIL: signalfd4: errno=%d (%s)\n",
                errno, strerror(errno));
        return EXIT_FAILURE;
    }

    uint64_t value = 1;
    errno = 0;
    ssize_t result = write(signal_fd, &value, sizeof(value));
    int write_errno = errno;
    close(signal_fd);

    if (result != -1 || write_errno != EINVAL) {
        fprintf(stderr,
                "FAIL: signalfd write: result=%zd errno=%d (%s), expected EINVAL\n",
                result, write_errno, strerror(write_errno));
        return EXIT_FAILURE;
    }

    puts("PASS: signalfd write returns EINVAL");
    puts("STARRY_SIGNALFD_WRITE_EINVAL_PASSED");
    puts("STARRY_GROUPED_TEST_PASSED: bugfix-signalfd-write-einval");
    return EXIT_SUCCESS;
}
