/*
 * test-aarch64-stack-size — 验证用户栈大小至少 8 MiB,
 * 同时 RLIMIT_STACK 与实际映射一致。
 *
 * StarryOS aarch64 上 USER_STACK_SIZE 之前是 512 KiB, 没法跑 GTK/weston
 * 这类在栈上分配大缓冲区的程序; getrlimit(RLIMIT_STACK) 返回值还和实际
 * 映射不一致, PostgreSQL 的 max_stack_depth 计算因此偏大触发踩别人页
 * 的崩溃。
 *
 * 用 6 MiB 局部数组逼一下栈, 同时校验 getrlimit(RLIMIT_STACK) >= 8 MiB,
 * 任何一项不满足都判 FAIL。
 *
 * 非 aarch64 架构原样跳过。
 */

#include "test_framework.h"
#include <sys/resource.h>
#include <stdint.h>

#define BIG_BUF_BYTES (6u * 1024u * 1024u)

#if defined(__aarch64__)
/* 不让编译器把局部数组优化掉。 */
__attribute__((noinline))
static void touch_first_last(volatile char *buf, size_t n)
{
    buf[0] = (char)0xa5;
    buf[n - 1] = (char)0x5a;
}
#endif

int main(void)
{
    TEST_START("aarch64-stack-size");

#if !defined(__aarch64__)
    printf("  SKIP | non-aarch64 target\n");
    TEST_DONE();
#else
    /* 1. RLIMIT_STACK 应该至少 8 MiB, 与 USER_STACK_SIZE 一致。 */
    struct rlimit rl = {0};
    int rc = getrlimit(RLIMIT_STACK, &rl);
    CHECK(rc == 0, "getrlimit(RLIMIT_STACK) succeeded");
    if (rc == 0) {
        CHECK(rl.rlim_cur >= 8u * 1024u * 1024u,
              "RLIMIT_STACK current >= 8 MiB");
    }

    /* 2. 在栈上放 6 MiB 数组, 写首尾两字节, 不能触发 SIGSEGV。
     *  小于 8 MiB 留一点 frame 余量, 避免内核栈帧 + libc 启动占的几页
     *  把 8 MiB 顶满。 */
    char buf[BIG_BUF_BYTES];
    touch_first_last(buf, BIG_BUF_BYTES);
    CHECK(buf[0] == (char)0xa5, "first byte of 6 MiB stack buffer is writable");
    CHECK(buf[BIG_BUF_BYTES - 1] == (char)0x5a,
          "last byte of 6 MiB stack buffer is writable");

    TEST_DONE();
#endif
}
