/*
 * test_time_syscalls.c -- 时间相关 syscall 综合测试
 *
 * 测试内容：
 *   1. clock_gettime: CLOCK_REALTIME/COARSE, CLOCK_MONOTONIC/RAW/COARSE/BOOTTIME,
 *      CLOCK_PROCESS/THREAD_CPUTIME_ID, 非法 clock_id → EINVAL, tv_nsec 范围
 *   2. gettimeofday: 正常返回, tv_usec 范围, NULL timezone
 *   3. nanosleep: 短睡眠, NULL rem, 0 值睡眠
 *   4. clock_nanosleep: CLOCK_REALTIME/MONOTONIC, TIMER_ABSTIME, 不支持 clock → EINVAL
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <time.h>
#include <sys/time.h>
#include <errno.h>
#include <unistd.h>

/* helper: timespec → nanoseconds */
static long long ts_to_ns(struct timespec *ts)
{
    return (long long)ts->tv_sec * 1000000000LL + ts->tv_nsec;
}

/* ==================== clock_gettime ==================== */
static void test_clock_gettime(void)
{
    printf("--- clock_gettime ---\n");

    /* CLOCK_REALTIME */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_REALTIME, &ts), 0, "CLOCK_REALTIME 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_REALTIME tv_sec >= 0");
        CHECK(ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000L, "CLOCK_REALTIME tv_nsec ∈ [0, 1e9)");
    }

    /* CLOCK_REALTIME_COARSE */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_REALTIME_COARSE, &ts), 0, "CLOCK_REALTIME_COARSE 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_REALTIME_COARSE tv_sec >= 0");
        CHECK(ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000L, "CLOCK_REALTIME_COARSE tv_nsec ∈ [0, 1e9)");
    }

    /* CLOCK_MONOTONIC */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &ts), 0, "CLOCK_MONOTONIC 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_MONOTONIC tv_sec >= 0");
        CHECK(ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000L, "CLOCK_MONOTONIC tv_nsec ∈ [0, 1e9)");
    }

    /* CLOCK_MONOTONIC_RAW */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC_RAW, &ts), 0, "CLOCK_MONOTONIC_RAW 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_MONOTONIC_RAW tv_sec >= 0");
    }

    /* CLOCK_MONOTONIC_COARSE */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC_COARSE, &ts), 0, "CLOCK_MONOTONIC_COARSE 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_MONOTONIC_COARSE tv_sec >= 0");
    }

    /* CLOCK_BOOTTIME */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_BOOTTIME, &ts), 0, "CLOCK_BOOTTIME 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_BOOTTIME tv_sec >= 0");
    }

    /* CLOCK_PROCESS_CPUTIME_ID */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts), 0,
                  "CLOCK_PROCESS_CPUTIME_ID 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_PROCESS_CPUTIME_ID tv_sec >= 0");
    }

    /* CLOCK_THREAD_CPUTIME_ID */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts), 0,
                  "CLOCK_THREAD_CPUTIME_ID 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_THREAD_CPUTIME_ID tv_sec >= 0");
    }

    /* 单调递增检查 */
    {
        struct timespec t1, t2;
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &t1), 0, "单调递增: 取 t1");
        volatile int x = 0;
        for (int i = 0; i < 100000; i++) x += i;
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &t2), 0, "单调递增: 取 t2");
        CHECK(ts_to_ns(&t2) >= ts_to_ns(&t1), "CLOCK_MONOTONIC 单调递增");
    }

    /* 非法 clock_id → EINVAL */
    CHECK_ERR(clock_gettime(9999, &(struct timespec){0}), EINVAL,
              "非法 clock_id 9999 → EINVAL");
    CHECK_ERR(clock_gettime(-1, &(struct timespec){0}), EINVAL,
              "负数 clock_id -1 → EINVAL");
}

/* ==================== gettimeofday ==================== */
static void test_gettimeofday(void)
{
    printf("--- gettimeofday ---\n");

    /* 正常调用 */
    {
        struct timeval tv = {0, 0};
        CHECK_RET(gettimeofday(&tv, NULL), 0, "gettimeofday 成功");
        CHECK(tv.tv_sec > 0, "gettimeofday tv_sec > 0");
        CHECK(tv.tv_usec >= 0 && tv.tv_usec < 1000000L, "gettimeofday tv_usec ∈ [0, 1e6)");
    }

    /* 与 CLOCK_REALTIME 交叉校验 */
    {
        struct timeval tv;
        struct timespec ts;
        gettimeofday(&tv, NULL);
        clock_gettime(CLOCK_REALTIME, &ts);
        long long diff = ts.tv_sec - tv.tv_sec;
        CHECK(diff >= 0 && diff <= 2, "gettimeofday 与 CLOCK_REALTIME 差值 ≤ 2s");
    }
}

/* ==================== nanosleep ==================== */
static void test_nanosleep(void)
{
    printf("--- nanosleep ---\n");

    /* 短睡眠 10ms */
    {
        struct timespec req = {0, 10000000L};
        struct timespec rem = {-1, -1};
        CHECK_RET(nanosleep(&req, &rem), 0, "nanosleep 10ms 成功");
    }

    /* NULL rem 指针 */
    {
        struct timespec req = {0, 10000000L};
        CHECK_RET(nanosleep(&req, NULL), 0, "nanosleep NULL rem 成功");
    }

    /* 0 值睡眠 */
    {
        struct timespec req = {0, 0};
        CHECK_RET(nanosleep(&req, NULL), 0, "nanosleep 0 成功");
    }

    /* nanosleep 后时间确实流逝 */
    {
        struct timespec before, after, req = {0, 10000000L};
        clock_gettime(CLOCK_MONOTONIC, &before);
        nanosleep(&req, NULL);
        clock_gettime(CLOCK_MONOTONIC, &after);
        long long elapsed = ts_to_ns(&after) - ts_to_ns(&before);
        CHECK(elapsed >= 9000000LL, "nanosleep 至少经过 ~9ms");
    }
}

/* ==================== clock_nanosleep ==================== */
static void test_clock_nanosleep(void)
{
    printf("--- clock_nanosleep ---\n");

    /* CLOCK_MONOTONIC 相对睡眠 */
    {
        struct timespec req = {0, 10000000L};
        CHECK_RET(clock_nanosleep(CLOCK_MONOTONIC, 0, &req, NULL), 0,
                  "clock_nanosleep CLOCK_MONOTONIC 成功");
    }

    /* CLOCK_REALTIME 相对睡眠 */
    {
        struct timespec req = {0, 10000000L};
        CHECK_RET(clock_nanosleep(CLOCK_REALTIME, 0, &req, NULL), 0,
                  "clock_nanosleep CLOCK_REALTIME 成功");
    }

    /* TIMER_ABSTIME: 绝对时间睡眠 */
    {
        struct timespec now, abs_time;
        clock_gettime(CLOCK_MONOTONIC, &now);
        abs_time.tv_sec = now.tv_sec;
        abs_time.tv_nsec = now.tv_nsec + 5000000L;
        if (abs_time.tv_nsec >= 1000000000L) {
            abs_time.tv_sec += 1;
            abs_time.tv_nsec -= 1000000000L;
        }
        CHECK_RET(clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &abs_time, NULL), 0,
                  "clock_nanosleep TIMER_ABSTIME 成功");
    }

    /* 不支持的 clock → EINVAL */
    {
        struct timespec req = {0, 1000000L};
        int ret = clock_nanosleep(9999, 0, &req, NULL);
        CHECK(ret == EINVAL, "clock_nanosleep 不支持的 clock_id → EINVAL");
    }
}

/* ==================== main ==================== */
int main(void)
{
    TEST_START("time-syscalls");

    test_clock_gettime();
    test_gettimeofday();
    test_nanosleep();
    test_clock_nanosleep();

    TEST_DONE();
}
