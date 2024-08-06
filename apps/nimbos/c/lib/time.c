#include <time.h>

#include "syscall.h"

int clock_nanosleep(clockid_t clk, int flags, const struct timespec *req,
                    struct timespec *rem)
{
    return syscall(SYS_clock_nanosleep, clk, flags, req);
}

int nanosleep(const struct timespec *req, struct timespec *rem)
{
    return clock_nanosleep(CLOCK_REALTIME, 0, req, rem);
}

int usleep(unsigned useconds)
{
    struct timespec tv = {.tv_sec = useconds / 1000000,
                          .tv_nsec = (useconds % 1000000) * 1000};
    return nanosleep(&tv, &tv);
}

unsigned sleep(unsigned seconds)
{
    struct timespec tv = {.tv_sec = seconds / 1000000, .tv_nsec = 0};
    if (nanosleep(&tv, &tv))
        return tv.tv_sec;
    return 0;
}

int clock_gettime(clockid_t clk, struct timespec *ts)
{
    return syscall(SYS_clock_gettime, clk, ts);
}

int gettimeofday(struct timeval *restrict tv, void *restrict tz)
{
    struct timespec ts;
    if (!tv)
        return 0;
    clock_gettime(CLOCK_REALTIME, &ts);
    tv->tv_sec = ts.tv_sec;
    tv->tv_usec = (int)ts.tv_nsec / 1000;
    return 0;
}
