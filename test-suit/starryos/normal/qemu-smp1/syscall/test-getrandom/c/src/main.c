/*
 * test_getrandom.c — 验证 getrandom 系统调用的 flags 校验、地址校验及边界语义。
 *
 * 覆盖场景：
 *   1. 基本随机数填充，返回长度必须等于请求长度。
 *   2. len=0 直接返回 0，包括 buf 为 NULL 或明显坏地址的情况。
 *   3. 覆盖所有 Linux 有效 flag 组合：
 *      0、GRND_NONBLOCK、GRND_RANDOM、GRND_RANDOM|GRND_NONBLOCK、
 *      GRND_INSECURE、GRND_INSECURE|GRND_NONBLOCK。
 *   4. 无效 flags（未知位）和互斥组合（GRND_RANDOM|GRND_INSECURE）
 *      必须返回 EINVAL，且不能改写用户缓冲区。
 *   5. 非零长度的 NULL/坏地址必须返回 EFAULT。
 *   6. 错误优先级：无效 flags 应先于用户地址写入检查返回 EINVAL。
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <sys/random.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef GRND_INSECURE
#define GRND_INSECURE 0x0004
#endif

#define UNKNOWN_GETRANDOM_FLAG 0x80000000U

/* 通过 syscall() 直接调用 getrandom，避免 glibc 封装差异 */
static ssize_t my_getrandom(void *buf, size_t len, unsigned int flags) {
    return syscall(SYS_getrandom, buf, len, flags);
}

static int all_bytes_equal(const unsigned char *buf, size_t len, unsigned char value) {
    for (size_t i = 0; i < len; i++) {
        if (buf[i] != value) {
            return 0;
        }
    }
    return 1;
}

static void check_full_random_read(unsigned int flags, const char *name) {
    unsigned char buf[32];
    memset(buf, 0, sizeof(buf));
    CHECK_RET(my_getrandom(buf, sizeof(buf), flags), (ssize_t)sizeof(buf), name);
    CHECK(!all_bytes_equal(buf, sizeof(buf), 0), "buffer should not remain all zero");
}

static void check_random_nonblock(unsigned int flags, const char *name) {
    unsigned char buf[32];
    memset(buf, 0, sizeof(buf));
    errno = 0;
    ssize_t n = my_getrandom(buf, sizeof(buf), flags);
    CHECK(n == (ssize_t)sizeof(buf) || (n == -1 && errno == EAGAIN), name);
    if (n == (ssize_t)sizeof(buf)) {
        CHECK(!all_bytes_equal(buf, sizeof(buf), 0), "successful random read fills buffer");
    }
}

int main(void) {
    TEST_START("getrandom");

    /* 1. 基本功能：填充 32 字节，返回值应为 32 */
    {
        check_full_random_read(0, "basic fill returns requested length");
    }

    /* 2. len=0 时直接返回 0，不做任何用户地址访问 */
    {
        unsigned char buf[1];
        CHECK_RET(my_getrandom(buf, 0, 0), 0, "len=0 returns 0");
        CHECK_RET(my_getrandom(NULL, 0, 0), 0, "len=0 accepts NULL buffer");
        CHECK_RET(my_getrandom((void *)1, 0, 0), 0, "len=0 accepts bad buffer");
    }

    /* 3. 所有有效 flag 组合 */
    {
        check_full_random_read(GRND_NONBLOCK, "GRND_NONBLOCK is accepted");
        check_full_random_read(GRND_RANDOM, "GRND_RANDOM is accepted");
        check_random_nonblock(GRND_RANDOM | GRND_NONBLOCK,
                              "GRND_RANDOM|GRND_NONBLOCK returns data or EAGAIN");
        check_full_random_read(GRND_INSECURE, "GRND_INSECURE is accepted");
        check_full_random_read(GRND_INSECURE | GRND_NONBLOCK,
                               "GRND_INSECURE|GRND_NONBLOCK is accepted");
    }

    /* 4. 无效 flags：未知位必须返回 EINVAL，且不能改写用户缓冲区 */
    {
        unsigned char buf[16];
        memset(buf, 0xA5, sizeof(buf));
        CHECK_ERR(my_getrandom(buf, sizeof(buf), UNKNOWN_GETRANDOM_FLAG), EINVAL,
                  "unknown flag returns EINVAL");
        CHECK(all_bytes_equal(buf, sizeof(buf), 0xA5),
              "unknown flag leaves buffer unchanged");
    }

    /* 5. GRND_INSECURE 与 GRND_RANDOM 互斥，同时使用返回 EINVAL */
    {
        unsigned char buf[16];
        memset(buf, 0x5A, sizeof(buf));
        CHECK_ERR(my_getrandom(buf, sizeof(buf), GRND_INSECURE | GRND_RANDOM), EINVAL,
                  "GRND_INSECURE|GRND_RANDOM 互斥返回 EINVAL");
        CHECK(all_bytes_equal(buf, sizeof(buf), 0x5A),
              "mutually-exclusive flags leave buffer unchanged");
    }

    /* 6. 非零长度的坏地址必须返回 EFAULT */
    {
        CHECK_ERR(my_getrandom(NULL, 1, 0), EFAULT, "NULL buffer with len>0 returns EFAULT");
        CHECK_ERR(my_getrandom((void *)1, 1, 0), EFAULT, "bad buffer with len>0 returns EFAULT");
    }

    /* 7. 错误优先级：无效 flags 应先于用户地址检查 */
    {
        CHECK_ERR(my_getrandom((void *)1, 16, UNKNOWN_GETRANDOM_FLAG), EINVAL,
                  "invalid flags take precedence over bad address");
        CHECK_ERR(my_getrandom(NULL, 16, GRND_INSECURE | GRND_RANDOM), EINVAL,
                  "mutually-exclusive flags take precedence over NULL address");
    }

    /* 8. 较大但仍合理的请求长度，避免只实现了小 buffer 的假阳性 */
    {
        unsigned char buf[4096];
        memset(buf, 0, sizeof(buf));
        CHECK_RET(my_getrandom(buf, sizeof(buf), 0), (ssize_t)sizeof(buf),
                  "large 4096-byte request returns full length");
        CHECK(!all_bytes_equal(buf, sizeof(buf), 0),
              "large request fills buffer");
    }

    TEST_DONE();
}
