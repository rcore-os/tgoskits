#define _GNU_SOURCE
#include "test_framework.h"
#include <stdint.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#if defined(__x86_64__)
#define SYS_SETTIMEOFDAY_NR 164
#define SYS_CLOCK_SETTIME_NR 227
#elif defined(__riscv) || defined(__aarch64__) || defined(__loongarch64)
#define SYS_SETTIMEOFDAY_NR 170
#define SYS_CLOCK_SETTIME_NR 112
#else
#error "unsupported architecture for clock/settime test"
#endif

int main(void)
{
    TEST_START("clock_settime / settimeofday");

    /* clock_settime: valid set, then clock_gettime observes it. */
    {
        struct timespec ts;
        ts.tv_sec = 1600000000;
        ts.tv_nsec = 123456789;
        CHECK_RET(syscall(SYS_CLOCK_SETTIME_NR, 0 /* CLOCK_REALTIME */, &ts), 0,
                  "clock_settime(CLOCK_REALTIME, valid)");

        memset(&ts, 0, sizeof(ts));
        CHECK_RET(clock_gettime(CLOCK_REALTIME, &ts), 0, "clock_gettime after set");
        CHECK(ts.tv_sec >= 1600000000 && ts.tv_sec <= 1600000002,
              "clock_gettime is within 2s of the set value");

        /* ABI validation. */
        ts.tv_sec = 1600000000;
        ts.tv_nsec = 1000000000;
        CHECK_ERR(syscall(SYS_CLOCK_SETTIME_NR, 0, &ts), EINVAL,
                  "clock_settime(nsec out of range) -> EINVAL");
        CHECK_ERR(syscall(SYS_CLOCK_SETTIME_NR, 1 /* CLOCK_MONOTONIC */, &ts), EINVAL,
                  "clock_settime(non-realtime clock) -> EINVAL");
    }

    /* settimeofday: valid timeval sets the wall clock. */
    {
        struct timeval tv;
        tv.tv_sec = 1600000000;
        tv.tv_usec = 500000;
        CHECK_RET(syscall(SYS_SETTIMEOFDAY_NR, &tv, NULL), 0,
                  "settimeofday(valid tv, NULL tz)");
        CHECK_ERR(syscall(SYS_SETTIMEOFDAY_NR, NULL, NULL), EINVAL,
                  "settimeofday(NULL, NULL) -> EINVAL");
        tv.tv_usec = 1000000;
        CHECK_ERR(syscall(SYS_SETTIMEOFDAY_NR, &tv, NULL), EINVAL,
                  "settimeofday(usec out of range) -> EINVAL");
    }

    TEST_DONE();
}
