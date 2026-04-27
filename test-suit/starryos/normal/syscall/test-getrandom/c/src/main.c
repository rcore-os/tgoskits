/*
 * test_getrandom.c — 验证 getrandom 系统调用的 flags 校验及基本功能。
 *
 * 覆盖场景：
 *   1. 基本随机数填充
 *   2. len=0 返回 0
 *   3. GRND_RANDOM flag 正常工作
 *   4. 无效 flags（含未知位）返回 EINVAL
 *   5. GRND_INSECURE 与 GRND_RANDOM 互斥，同时传入返回 EINVAL
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <sys/random.h>
#include <sys/syscall.h>
#include <unistd.h>

/* 通过 syscall() 直接调用 getrandom，避免 glibc 封装差异 */
static ssize_t my_getrandom(void *buf, size_t len, unsigned int flags) {
    return syscall(SYS_getrandom, buf, len, flags);
}

int main(void) {
    TEST_START("getrandom");

    /* 基本功能：填充 32 字节，返回值应为 32 */
    {
        unsigned char buf[32];
        memset(buf, 0, sizeof(buf));
        CHECK_RET(my_getrandom(buf, sizeof(buf), 0), 32, "基本填充返回32字节");

        /* 全零概率极低，用于粗略验证随机性 */
        int all_zero = 1;
        for (int i = 0; i < 32; i++) {
            if (buf[i] != 0) { all_zero = 0; break; }
        }
        CHECK(!all_zero, "缓冲区不应全为零");
    }

    /* len=0 时直接返回 0，不做任何操作 */
    {
        unsigned char buf[1];
        CHECK_RET(my_getrandom(buf, 0, 0), 0, "len=0 返回0");
    }

    /* GRND_RANDOM flag：从 /dev/random 读取。
     * 使用 GRND_NONBLOCK 避免在低熵环境（QEMU/早期启动）下阻塞。 */
    {
        unsigned char buf[16];
        ssize_t n = my_getrandom(buf, sizeof(buf), GRND_RANDOM | GRND_NONBLOCK);
        CHECK(n > 0 || (n == -1 && errno == EAGAIN),
              "GRND_RANDOM|GRND_NONBLOCK: 返回数据或 EAGAIN");
    }

    /* 无效 flags：0xFF 含有未定义位，Linux 内核拒绝并返回 EINVAL */
    {
        unsigned char buf[16];
        CHECK_ERR(my_getrandom(buf, sizeof(buf), 0xFF), EINVAL, "未知 flags 返回 EINVAL");
    }

    /* GRND_INSECURE 与 GRND_RANDOM 互斥，同时使用返回 EINVAL */
    {
        unsigned char buf[16];
        CHECK_ERR(my_getrandom(buf, sizeof(buf), GRND_INSECURE | GRND_RANDOM), EINVAL,
                  "GRND_INSECURE|GRND_RANDOM 互斥返回 EINVAL");
    }

    TEST_DONE();
}
