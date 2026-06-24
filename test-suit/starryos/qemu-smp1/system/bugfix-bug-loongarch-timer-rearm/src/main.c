#define _GNU_SOURCE

#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static int sleep_for_ns(long ns)
{
    struct timespec ts = {
        .tv_sec = ns / 1000000000L,
        .tv_nsec = ns % 1000000000L,
    };

    while (nanosleep(&ts, &ts) != 0) {
        if (errno != EINTR)
            return -1;
    }
    return 0;
}

static int wait_timerfd_once(int fd)
{
    uint64_t expirations = 0;
    ssize_t n = read(fd, &expirations, sizeof expirations);
    return n == (ssize_t)sizeof expirations && expirations >= 1 ? 0 : -1;
}

int main(void)
{
    TEST_START("loongarch64 one-shot timer rearm");

    /*
     * LoongArch uses one-shot timer events. The IRQ path must acknowledge the
     * current event before dispatching into code that programs the next event;
     * otherwise a late ACK can clear a freshly pending event and leave sleeps
     * or timerfd reads blocked forever. Short repeated sleeps exercise the
     * kernel's timer reprogramming path without adding much wall-clock time.
     */
    for (int i = 0; i < 16; i++) {
        char msg[96];
        snprintf(msg, sizeof msg, "nanosleep rearm iteration %d completes", i);
        CHECK(sleep_for_ns(2 * 1000 * 1000L) == 0, msg);
    }

    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(fd >= 0, "timerfd_create CLOCK_MONOTONIC succeeds");

    struct itimerspec spec = {
        .it_interval = {0, 0},
        .it_value = {0, 3 * 1000 * 1000L},
    };
    for (int i = 0; fd >= 0 && i < 8; i++) {
        char set_msg[96];
        snprintf(set_msg, sizeof set_msg, "timerfd_settime one-shot iteration %d", i);
        CHECK_RET(timerfd_settime(fd, 0, &spec, NULL), 0, set_msg);

        char read_msg[96];
        snprintf(read_msg, sizeof read_msg, "timerfd one-shot iteration %d expires", i);
        CHECK(wait_timerfd_once(fd) == 0, read_msg);
    }

    if (fd >= 0)
        close(fd);

    TEST_DONE();
}
