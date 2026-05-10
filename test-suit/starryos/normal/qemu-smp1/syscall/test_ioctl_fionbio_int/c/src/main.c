/*
 * test_ioctl_fionbio_int.c — ioctl FIONBIO
 *
 * 测试策略：验证 ioctl(FIONBIO) 按整型解释 arg（非单字节），且任意非零均开启 O_NONBLOCK
 *
 * 覆盖范围：
 *   正向：*arg=2（非零 int，低字节非 0/1）应成功开启非阻塞
 *   正向：*arg=256（非零 int，低字节为 0）应成功开启非阻塞
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* ========== 内联测试框架 ========== */

static int test_passed = 0;
static int test_failed = 0;

#define TEST_START(name) \
    do { \
        printf("[TEST] %s\n", (name)); \
        test_passed = 0; \
        test_failed = 0; \
    } while(0)

#define CHECK(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("  [FAIL] %s\n", (msg)); \
            test_failed++; \
        } else { \
            printf("  [OK] %s\n", (msg)); \
            test_passed++; \
        } \
    } while(0)

#define CHECK_RET(val, expected, msg) \
    do { \
        long long _v = (long long)(val); \
        long long _e = (long long)(expected); \
        if (_v != _e) { \
            printf("  [FAIL] %s (got %lld, expected %lld)\n", (msg), _v, _e); \
            test_failed++; \
        } else { \
            printf("  [OK] %s (= %lld)\n", (msg), _v); \
            test_passed++; \
        } \
    } while(0)

#define TEST_DONE() \
    do { \
        printf("\n=== 结果: %d 通过, %d 失败 ===\n", test_passed, test_failed); \
        if (test_failed == 0) { printf("TEST PASSED\n"); } \
        return (test_failed == 0) ? EXIT_SUCCESS : EXIT_FAILURE; \
    } while(0)

/* ========== 测试代码 ========== */

int main(void)
{
    TEST_START("ioctl: FIONBIO reads full int value");

    /* 1. FIONBIO with *arg=2 */
    {
        int p[2];
        int r = pipe(p);
        CHECK(r == 0, "pipe 创建成功");
        if (r != 0) { TEST_DONE(); }

        int v = 2;
        errno = 0;
        r = ioctl(p[0], FIONBIO, &v);
        CHECK(r == 0, "FIONBIO *arg=2 成功");

        /* 验证 O_NONBLOCK 确实被设置 */
        int flags = fcntl(p[0], F_GETFL);
        CHECK(flags >= 0, "F_GETFL 成功");
        CHECK((flags & O_NONBLOCK) != 0, "O_NONBLOCK 已设置 (*arg=2)");

        close(p[0]);
        close(p[1]);
    }

    /* 2. FIONBIO with *arg=256 (低字节为 0，但 int 整体非零) */
    {
        int p[2];
        int r = pipe(p);
        CHECK(r == 0, "pipe 创建成功 (256 测试)");
        if (r != 0) { TEST_DONE(); }

        int v = 256;
        errno = 0;
        r = ioctl(p[0], FIONBIO, &v);
        CHECK(r == 0, "FIONBIO *arg=256 成功");

        int flags = fcntl(p[0], F_GETFL);
        CHECK(flags >= 0, "F_GETFL 成功 (256 测试)");
        CHECK((flags & O_NONBLOCK) != 0, "O_NONBLOCK 已设置 (*arg=256)");

        close(p[0]);
        close(p[1]);
    }

    /* 3. FIONBIO with *arg=0 关闭非阻塞 */
    {
        int p[2];
        int r = pipe(p);
        CHECK(r == 0, "pipe 创建成功 (关闭测试)");
        if (r != 0) { TEST_DONE(); }

        /* 先开启 */
        int v = 1;
        ioctl(p[0], FIONBIO, &v);

        /* 再关闭 */
        v = 0;
        errno = 0;
        r = ioctl(p[0], FIONBIO, &v);
        CHECK(r == 0, "FIONBIO *arg=0 关闭成功");

        int flags = fcntl(p[0], F_GETFL);
        CHECK(flags >= 0, "F_GETFL 成功 (关闭测试)");
        CHECK((flags & O_NONBLOCK) == 0, "O_NONBLOCK 已关闭 (*arg=0)");

        close(p[0]);
        close(p[1]);
    }

    TEST_DONE();
}
