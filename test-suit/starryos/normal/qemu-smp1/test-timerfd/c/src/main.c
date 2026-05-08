/* timerfd regression test.
 *
 * Exercises:
 *   - timerfd_create(CLOCK_MONOTONIC, 0)
 *   - timerfd_settime with a 100ms one-shot
 *   - read returns u64 = 1 (one expiration)
 *   - timerfd_settime with 50ms repeating interval
 *   - after ~200ms at least 3 expirations are delivered
 *   - poll() reports readable after the first tick
 *   - CLOCK_REALTIME + TFD_TIMER_ABSTIME is rejected with EINVAL
 */
#include "test_framework.h"

#include <poll.h>
#include <stdint.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    TEST_START("timerfd");

    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(fd >= 0, "create CLOCK_MONOTONIC");

    /* one-shot, 100 ms */
    struct itimerspec spec = {
        .it_interval = {0, 0},
        .it_value    = {0, 100 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(fd, 0, &spec, NULL), 0, "settime oneshot 100ms");

    uint64_t expirations = 0;
    ssize_t n = read(fd, &expirations, sizeof(expirations));
    CHECK(n == (ssize_t)sizeof(expirations), "read oneshot returned 8 bytes");
    CHECK(expirations == 1, "oneshot expiration count == 1");

    /* periodic, 50 ms */
    struct itimerspec periodic = {
        .it_interval = {0, 50 * 1000 * 1000},
        .it_value    = {0, 50 * 1000 * 1000},
    };
    CHECK_RET(timerfd_settime(fd, 0, &periodic, NULL), 0, "settime periodic 50ms");

    /* poll() should become readable */
    struct pollfd pf = {.fd = fd, .events = POLLIN};
    int p = poll(&pf, 1, 500);
    CHECK(p == 1 && (pf.revents & POLLIN), "poll() reports POLLIN");

    /* let it run a bit longer then read */
    struct timespec sleep_for = {0, 200 * 1000 * 1000};
    nanosleep(&sleep_for, NULL);

    expirations = 0;
    n = read(fd, &expirations, sizeof(expirations));
    CHECK(n == (ssize_t)sizeof(expirations), "read periodic returned 8 bytes");
    CHECK(expirations >= 3, "periodic expiration count >= 3");

    /* disarm */
    struct itimerspec disarm = {0};
    CHECK_RET(timerfd_settime(fd, 0, &disarm, NULL), 0, "settime disarm");

    close(fd);

    /* CLOCK_REALTIME + TFD_TIMER_ABSTIME must be rejected: this kernel's
     * wall_time() is monotonic-equivalent, treating absolute REALTIME as
     * monotonic would give a ~54-year skew. */
    int rfd = timerfd_create(CLOCK_REALTIME, 0);
    CHECK(rfd >= 0, "create CLOCK_REALTIME");
    struct itimerspec abs_spec = {
        .it_interval = {0, 0},
        .it_value    = {0, 100 * 1000 * 1000},
    };
    CHECK_ERR(timerfd_settime(rfd, TFD_TIMER_ABSTIME, &abs_spec, NULL), EINVAL,
              "REALTIME + ABSTIME rejected with EINVAL");
    close(rfd);

    TEST_DONE();
}
