#pragma once

/* 必须在最前面定义，确保 pipe2/gettid 等可用 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

/*
 * StarryOS Syscall Test Framework
 *
 * 极简独立测试框架：每个文件测一个 syscall，独立编译运行。
 * 目标：出错时精确定位到 源文件:行号 -> 哪个调用 -> 什么结果
 *
 * 用法:
 *   TEST_START("测试名");
 *   CHECK(call == expected, "描述");
 *   CHECK_ERR(call, EBADF, "描述");
 *   TEST_DONE();
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

static int __pass = 0;
static int __fail = 0;

/* ---- 核心: 带文件名+行号的检查宏 ---- */

/* 检查条件为真 */
#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* 检查 syscall 返回特定值 */
#define CHECK_RET(call, expected, msg) do {                             \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    long _e = (long)(expected);                                         \
    if (_r == _e) {                                                     \
        printf("  PASS | %s:%d | %s (ret=%ld)\n",                      \
               __FILE__, __LINE__, msg, _r);                            \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected=%ld got=%ld | errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, _e, _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* 检查 syscall 失败且 errno 符合预期 */
#define CHECK_ERR(call, exp_errno, msg) do {                            \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    if (_r == -1 && errno == (exp_errno)) {                             \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",         \
               __FILE__, __LINE__, msg, errno);                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

/* ---- 测试边界 ---- */
#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

#define TEST_DONE()                                                     \
    printf("------------------------------------------------\n");       \
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);              \
    printf("================================================\n\n");     \
    return __fail > 0 ? 1 : 0
