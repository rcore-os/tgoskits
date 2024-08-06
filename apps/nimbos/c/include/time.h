#ifndef __TIME_H__
#define __TIME_H__

typedef int clockid_t;
typedef long time_t;

struct timeval {
    time_t tv_sec;
    long tv_usec;
};

struct timespec {
    time_t tv_sec;
    long tv_nsec;
};

#define CLOCK_REALTIME  0
#define CLOCK_MONOTONIC 1

#define TIMER_ABSTIME 1

int clock_nanosleep(clockid_t clk, int flags, const struct timespec *req,
                    struct timespec *rem);
int nanosleep(const struct timespec *req, struct timespec *rem);
int clock_gettime(clockid_t clk, struct timespec *ts);
int gettimeofday(struct timeval *tv, void *tz);

#endif // __TIME_H__
