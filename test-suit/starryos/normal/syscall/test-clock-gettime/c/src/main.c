/*
 * test_clock_gettime.c -- clock_gettime 系统调用测试
 *
 * 测试内容：
 *   1. CLOCK_REALTIME 正常工作，返回正时间戳
 *   2. CLOCK_MONOTONIC 正常工作
 *   3. CLOCK_MONOTONIC 单调递增
 *   4. 非法 clock_id 应返回 EINVAL
 *   5. 负数 clock_id 应返回 EINVAL
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <time.h>
#include <errno.h>

int main(void)
{
    TEST_START("clock_gettime");

    /* CLOCK_REALTIME 应成功，tv_sec > 0 */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_REALTIME, &ts), 0, "CLOCK_REALTIME 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_REALTIME tv_sec >= 0");
        CHECK(ts.tv_nsec >= 0 && ts.tv_nsec < 1000000000L, "CLOCK_REALTIME tv_nsec 范围有效");
    }

    /* CLOCK_MONOTONIC 应成功 */
    {
        struct timespec ts = {0, 0};
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &ts), 0, "CLOCK_MONOTONIC 成功");
        CHECK(ts.tv_sec >= 0, "CLOCK_MONOTONIC tv_sec >= 0");
    }

    /* CLOCK_MONOTONIC 应单调递增 */
    {
        struct timespec t1, t2;
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &t1), 0, "单调递增: 取 t1");
        volatile int x = 0;
        for (int i = 0; i < 100000; i++) x += i;
        CHECK_RET(clock_gettime(CLOCK_MONOTONIC, &t2), 0, "单调递增: 取 t2");
        long ns1 = t1.tv_sec * 1000000000L + t1.tv_nsec;
        long ns2 = t2.tv_sec * 1000000000L + t2.tv_nsec;
        CHECK(ns2 >= ns1, "CLOCK_MONOTONIC 单调递增");
    }

    /* 非法 clock_id 应返回 EINVAL */
    CHECK_ERR(clock_gettime(9999, &(struct timespec){0, 0}), EINVAL,
              "非法 clock_id 9999 返回 EINVAL");

    /* 负数 clock_id 应返回 EINVAL */
    CHECK_ERR(clock_gettime(-1, &(struct timespec){0, 0}), EINVAL,
              "负数 clock_id -1 返回 EINVAL");

    TEST_DONE();
}
