#include "test.h"

#include <pthread.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>

static long elapsed_us(const struct timespec *before, const struct timespec *after)
{
    long sec = after->tv_sec - before->tv_sec;
    long nsec = after->tv_nsec - before->tv_nsec;

    if (nsec < 0) {
        sec -= 1;
        nsec += 1000000000L;
    }
    return sec * 1000000L + nsec / 1000L;
}

static void *usleep_thread(void *arg)
{
    (void)arg;
    usleep(20000);
    return NULL;
}

int arceos_c_test_pthread_sleep(char *reason, size_t reason_len)
{
    struct timespec before;
    struct timespec after;
    pthread_t thread;

    CHECK_RET(clock_gettime(0, &before), 0);
    usleep(50000);
    CHECK_RET(clock_gettime(0, &after), 0);
    CHECK_TRUE(elapsed_us(&before, &after) >= 40000);

    CHECK_RET(clock_gettime(0, &before), 0);
    sleep(1);
    CHECK_RET(clock_gettime(0, &after), 0);
    CHECK_TRUE(elapsed_us(&before, &after) >= 900000);

    CHECK_RET(pthread_create(&thread, NULL, usleep_thread, NULL), 0);
    CHECK_RET(pthread_join(thread, NULL), 0);
    puts("pthread_sleep: sleep APIs OK");
    return 0;
}
