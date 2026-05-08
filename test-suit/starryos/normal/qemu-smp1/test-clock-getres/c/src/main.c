#define _GNU_SOURCE
#include "test_framework.h"
#include <time.h>
#include <errno.h>
#include <string.h>

/*
 * clock_getres 对比测试:
 *   Linux/WSL 行为 vs StarryOS 行为
 *
 * 已知差异:
 *   1. StarryOS 硬编码分辨率为 1us (tv_nsec=1000), Linux 高精度时钟为 1ns
 *   2. StarryOS 只处理 CLOCK_REALTIME/CLOCK_MONOTONIC，其他 clock_id 只 warn 但不报错
 *   3. StarryOS 对无效 clock_id 也不返回 EINVAL (BUG)
 */

static int call_clock_getres(int clock_id, struct timespec *ts)
{
    errno = 0;
    return clock_getres(clock_id, ts);
}

int main(void)
{
    TEST_START("clock_getres");

    /* ================================================================
     * 1. CLOCK_REALTIME — 两者都应支持，但分辨率不同
     * ================================================================ */
    {
        struct timespec ts = {0};
        CHECK_RET(call_clock_getres(CLOCK_REALTIME, &ts), 0,
                  "CLOCK_REALTIME 应返回 0");
        CHECK(ts.tv_sec >= 0, "CLOCK_REALTIME tv_sec >= 0");
        CHECK(ts.tv_nsec > 0 && ts.tv_nsec < 1000000000,
              "CLOCK_REALTIME tv_nsec 在有效范围 (0, 1e9)");
        /* Linux: tv_nsec=1 (1ns 高精度). StarryOS: tv_nsec=1000 (1us). */
        CHECK(ts.tv_nsec <= 1000,
              "CLOCK_REALTIME 分辨率 <= 1us (Linux=1ns, Starry=1us)");
    }

    /* ================================================================
     * 2. CLOCK_MONOTONIC — 两者都应支持，但分辨率不同
     * ================================================================ */
    {
        struct timespec ts = {0};
        CHECK_RET(call_clock_getres(CLOCK_MONOTONIC, &ts), 0,
                  "CLOCK_MONOTONIC 应返回 0");
        CHECK(ts.tv_sec >= 0, "CLOCK_MONOTONIC tv_sec >= 0");
        CHECK(ts.tv_nsec > 0 && ts.tv_nsec < 1000000000,
              "CLOCK_MONOTONIC tv_nsec 在有效范围");
        CHECK(ts.tv_nsec <= 1000,
              "CLOCK_MONOTONIC 分辨率 <= 1us (Linux=1ns, Starry=1us)");
    }

    /* ================================================================
     * 3. 指针为 NULL — 应正确处理 (只返回值，不写内存)
     * ================================================================ */
    {
        /* Linux: clock_getres(clk_id, NULL) 返回 0, 合法行为 */
        CHECK_RET(call_clock_getres(CLOCK_REALTIME, NULL), 0,
                  "CLOCK_REALTIME with NULL 指针应返回 0");
    }

    /* ================================================================
     * 4. CLOCK_MONOTONIC_RAW — Linux 支持, StarryOS 只 warn 然后返回 Ok(1us)
     *    StarryOS 应返回 EINVAL 或正确实现
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_MONOTONIC_RAW, &ts);
        /* Linux: ret=0. StarryOS: ret=0 (但只是 warn, 并非真正支持) */
        CHECK_RET(ret, 0, "CLOCK_MONOTONIC_RAW 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_MONOTONIC_RAW tv_nsec > 0");
    }

    /* ================================================================
     * 5. CLOCK_REALTIME_COARSE — Linux 支持 (~4ms 精度), StarryOS 只 warn
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_REALTIME_COARSE, &ts);
        /* Linux: ret=0, tv_nsec=4000000. StarryOS: ret=0, tv_nsec=1000 (warn) */
        CHECK_RET(ret, 0, "CLOCK_REALTIME_COARSE 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_REALTIME_COARSE tv_nsec > 0");
        /* Linux 上 coarse 时钟分辨率为 4ms, 而 StarryOS 返回 1us —
         * 若分辨率 <= 1000ns 说明 StarryOS 未正确区分 coarse 时钟 */
        if (ts.tv_nsec <= 1000) {
            printf("  NOTE | CLOCK_REALTIME_COARSE tv_nsec=%ld (<=1000ns) "
                   "— likely StarryOS fallback, not real coarse resolution\n",
                   ts.tv_nsec);
        }
    }

    /* ================================================================
     * 6. CLOCK_MONOTONIC_COARSE — 同上
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_MONOTONIC_COARSE, &ts);
        CHECK_RET(ret, 0, "CLOCK_MONOTONIC_COARSE 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_MONOTONIC_COARSE tv_nsec > 0");
        if (ts.tv_nsec <= 1000) {
            printf("  NOTE | CLOCK_MONOTONIC_COARSE tv_nsec=%ld (<=1000ns) "
                   "— likely StarryOS fallback, not real coarse resolution\n",
                   ts.tv_nsec);
        }
    }

    /* ================================================================
     * 7. CLOCK_BOOTTIME — Linux 支持, StarryOS 只 warn
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_BOOTTIME, &ts);
        /* Linux: ret=0. StarryOS: ret=0 (warn fallback) */
        CHECK_RET(ret, 0, "CLOCK_BOOTTIME 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_BOOTTIME tv_nsec > 0");
    }

    /* ================================================================
     * 8. CLOCK_PROCESS_CPUTIME_ID — Linux 支持, StarryOS 只 warn
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_PROCESS_CPUTIME_ID, &ts);
        CHECK_RET(ret, 0, "CLOCK_PROCESS_CPUTIME_ID 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_PROCESS_CPUTIME_ID tv_nsec > 0");
    }

    /* ================================================================
     * 9. CLOCK_THREAD_CPUTIME_ID — Linux 支持, StarryOS 只 warn
     * ================================================================ */
    {
        struct timespec ts = {0};
        int ret = call_clock_getres(CLOCK_THREAD_CPUTIME_ID, &ts);
        CHECK_RET(ret, 0, "CLOCK_THREAD_CPUTIME_ID 应返回 0 (Linux 支持)");
        CHECK(ts.tv_nsec > 0, "CLOCK_THREAD_CPUTIME_ID tv_nsec > 0");
    }

    /* ================================================================
     * 10. 无效 clock_id (-1): Linux 返回 EINVAL, StarryOS 返回 0 (BUG!)
     * ================================================================ */
    {
        struct timespec ts = {0};
        /* Linux: ret=-1, errno=EINVAL. StarryOS: ret=0 (BUG) */
        CHECK_ERR(call_clock_getres(-1, &ts), EINVAL,
                  "clock_id=-1 应返回 EINVAL (StarryOS BUG: 返回 0)");
    }

    /* ================================================================
     * 11. 无效 clock_id (9999): Linux 返回 EINVAL, StarryOS 返回 0 (BUG!)
     * ================================================================ */
    {
        struct timespec ts = {0};
        /* Linux: ret=-1, errno=EINVAL. StarryOS: ret=0 (BUG) */
        CHECK_ERR(call_clock_getres(9999, &ts), EINVAL,
                  "clock_id=9999 应返回 EINVAL (StarryOS BUG: 返回 0)");
    }

    /* ================================================================
     * 12. 验证 tv_sec 分辨率为 0 (时钟分辨率不应达到秒级)
     * ================================================================ */
    {
        struct timespec ts = {0};
        CHECK_RET(call_clock_getres(CLOCK_REALTIME, &ts), 0,
                  "tv_sec 验证: CLOCK_REALTIME 应返回 0");
        CHECK(ts.tv_sec == 0,
              "时钟分辨率 tv_sec 应为 0 (不存在秒级时钟)");
    }

    TEST_DONE();
}
