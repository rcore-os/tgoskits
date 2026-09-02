#define _GNU_SOURCE

#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/timerfd.h>
#include <unistd.h>

static int passed;
static int failed;

static void check(int condition, const char *message)
{
    if (condition) {
        ++passed;
        printf("PASS: %s\n", message);
    } else {
        ++failed;
        printf("FAIL: %s\n", message);
    }
}

int main(void)
{
    struct itimerspec timer = {
        .it_interval = {0, 0},
        .it_value = {0, 50 * 1000 * 1000},
    };
    struct pollfd ready = {
        .fd = -1,
        .events = POLLIN,
        .revents = 0,
    };
    uint64_t expirations = 0;

    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
    check(fd >= 0, "create non-blocking monotonic timerfd");
    if (fd < 0) {
        printf("RESULT: %d passed / %d failed\n", passed, failed);
        return 1;
    }

    ready.fd = fd;
    check(timerfd_settime(fd, 0, &timer, NULL) == 0,
          "arm one-shot timerfd");
    check(poll(&ready, 1, 1000) == 1 && (ready.revents & POLLIN) != 0,
          "poll observes timerfd readiness");

    errno = 0;
    long result = syscall(SYS_read, fd, (void *)(uintptr_t)1, sizeof(expirations));
    check(result == -1 && errno == EFAULT,
          "ready timerfd read with bad buffer returns EFAULT");

    ready.revents = 0;
    check(poll(&ready, 1, 0) == 0,
          "EFAULT consumes timerfd readiness");

    errno = 0;
    result = syscall(SYS_read, fd, &expirations, sizeof(expirations));
    check(result == -1 && errno == EAGAIN,
          "read after EFAULT observes no pending expiration");

    check(close(fd) == 0, "close timerfd");

    printf("RESULT: %d passed / %d failed\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
        return 0;
    }
    return 1;
}
